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
//! so the staleness window can be crossed without a real clock, on the
//! shared harness in `tests/harness.rs`.
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

use core::pin::pin;
use core::task::{Context, Waker};

use embassy_time::MockDriver;
use ubos_core::units::FanPwm;

use super::tests_harness::{FakeFan, FakeTach, LOOP_MUTEX, drive, moderate_reading};
use super::{CONTROL_INTERVAL, handle, run_cooling, stale_timeout};

#[test]
fn staleness_failsafe_forces_and_releases_full_fan() {
    let _guard = LOOP_MUTEX.lock().expect("BUG: LOOP_MUTEX poisoned");
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
