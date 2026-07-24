// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Integration test for the staleness failsafe in [`super::run_cooling`].
//!
//! Drives the real `run_cooling` task against `embassy_time::MockDriver`
//! so the staleness window can be crossed without a real clock. The task
//! never returns, so it is polled manually instead of run under an
//! executor: each phase advances the mock clock or publishes a thermal
//! reading, then polls `run_cooling` and the metrics ingest task a bounded
//! number of times to let the resulting chain of awaits settle before the
//! commanded PWM is asserted through `super::handle`.
//!
//! All four phases share one `run_cooling` instance and the process-global
//! statics it reads and writes — the mock clock, config, and metrics
//! channel — so the test runs as a single function with sequential phases
//! rather than as independent `#[test]` cases.
//!
//! The "log once" behavior on the forced-full and recovery transitions is
//! guaranteed by the `forced_full` latch in `run_cooling` and is not asserted
//! here: this test runs under `--no-default-features`, with defmt off and no
//! log sink to count emissions against.

use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Waker};

use embassy_time::MockDriver;
use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_thermal::ThermalReading;

use super::{CONTROL_INTERVAL, FanOutput, TachReader, handle, run_cooling, stale_timeout};

/// Number of times each phase polls `run_cooling` and the metrics ingest
/// task, enough for the awaits a phase triggers to settle — the metric pushes
/// and any catch-up control ticks — while staying a fixed bound rather than an
/// open-ended poll loop. The bound assumes the default config's thermal sample
/// interval: a phase advances the clock by one `stale_timeout`, so raising
/// that interval enough that the window spans more than `POLL_BUDGET` control
/// ticks would under-poll and need a larger bound.
const POLL_BUDGET: usize = 64;

struct FakeFan;

impl FanOutput for FakeFan {
    fn set_pwm(&mut self, _pwm: FanPwm) {}
}

struct FakeTach;

impl TachReader<1> for FakeTach {
    const LABELS: [Option<&'static str>; 1] = [None];

    fn read_rpms(&mut self) -> [FanRpm; 1] {
        [FanRpm::from_rpm(0)]
    }
}

fn moderate_reading(sensor_fault: bool) -> ThermalReading {
    // Well below the default 67°C target: the PID drives the PWM toward
    // its floor rather than toward FULL, so a non-fault reading at this
    // temperature unambiguously demonstrates the fan leaving FULL.
    let temp = Temperature::from_millicelsius(50_000);
    ThermalReading {
        chip_max: temp,
        pid_temp: temp,
        timestamp_ms: 0,
        sensor_fault,
    }
}

/// Polls `cooling` and `ingest` up to [`POLL_BUDGET`] times so the awaits
/// triggered by the last clock advance or published reading settle. Both
/// futures never complete — `run_cooling` is `-> !`, `run_ingest` is `-> !`
/// and the channel never empties on its own — so the return value carries
/// no information; only the side effects on `super::STATE_WATCH` matter.
/// Takes the pinned futures by reborrow so the caller's `pin!` bindings
/// stay usable across the repeated calls one per test phase.
fn drive<Cooling: Future, Ingest: Future>(
    mut cooling: Pin<&mut Cooling>,
    mut ingest: Pin<&mut Ingest>,
    cx: &mut Context<'_>,
) {
    for _ in 0..POLL_BUDGET {
        let _ = cooling.as_mut().poll(cx);
        let _ = ingest.as_mut().poll(cx);
    }
}

#[test]
fn staleness_failsafe_forces_and_releases_full_fan() {
    let driver = MockDriver::get();
    driver.reset();

    let waker: &Waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let default_config = ubos_config::Config::default();
    let stale = stale_timeout(default_config.thermal.sample_interval_ms);
    let floor_pwm = FanPwm::from_percent(default_config.cooling.pid.min_fan_pwm);

    let mut cooling = pin!(run_cooling(FakeFan, FakeTach, ubos_thermal::handle()));
    let mut ingest = pin!(ubos_metrics::metrics::run_ingest());

    drive(cooling.as_mut(), ingest.as_mut(), &mut cx);
    let boot_state = handle().current().expect("run_cooling publishes at boot");
    assert_eq!(
        boot_state.pwm, floor_pwm,
        "before any reading the fan sits at the configured floor"
    );

    // Behavior 1: thermal silence past `stale_timeout`, with only control
    // ticks arriving, forces the fan to full.
    driver.advance(stale);
    drive(cooling.as_mut(), ingest.as_mut(), &mut cx);
    let stalled_state = handle().current().expect("published");
    assert_eq!(
        stalled_state.pwm,
        FanPwm::FULL,
        "thermal silence past stale_timeout must force the fan to full"
    );

    // Behavior 2: a fresh non-fault reading clears the forced_full latch
    // and hands the PWM back to the PID.
    ubos_thermal::publish_reading_for_test(moderate_reading(false));
    drive(cooling.as_mut(), ingest.as_mut(), &mut cx);
    let recovered_state = handle().current().expect("published");
    assert_eq!(
        recovered_state.pwm, floor_pwm,
        "a fresh reading well below target must clear the staleness latch and \
         resume PID control, which drives the PWM down to the min_fan_pwm floor"
    );

    // Behavior 3: silence one control tick short of the window must not
    // force full. This pins the `silence >= stale` boundary from below, so
    // the failsafe cannot fire one tick early.
    let short_silence = stale - CONTROL_INTERVAL;
    driver.advance(short_silence);
    drive(cooling.as_mut(), ingest.as_mut(), &mut cx);
    let short_silence_state = handle().current().expect("published");
    assert_eq!(
        short_silence_state.pwm, floor_pwm,
        "silence one tick short of stale_timeout must not force the fan to full; \
         PID control still holds the PWM at the min_fan_pwm floor"
    );

    // Behavior 4: a sensor_fault reading forces the fan to full on its
    // own, independent of the staleness latch, which is already clear at
    // this point.
    ubos_thermal::publish_reading_for_test(moderate_reading(true));
    drive(cooling.as_mut(), ingest.as_mut(), &mut cx);
    let fault_state = handle().current().expect("published");
    assert_eq!(
        fault_state.pwm,
        FanPwm::FULL,
        "a sensor_fault reading must force the fan to full"
    );
}
