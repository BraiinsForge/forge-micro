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

//! Integration test for `FanMode::Fixed` in [`super::run_cooling`], on the
//! shared harness in `tests/harness.rs`.
//!
//! All phases share one `run_cooling` instance and the statics it reads
//! and writes, so the test runs as a single function with sequential
//! phases rather than as independent `#[test]` cases. The thermal and
//! config channels it publishes on are its own, through [`Upstream`],
//! so no other test observes what this one sets.

// The crate is `#![no_std]`; explicitly opt back into std for the poisoned
// `LOOP_MUTEX` recovery below.
extern crate std;

use core::cell::Cell;
use core::pin::pin;

use embassy_time::MockDriver;
use ubos_config::{Config, FanMode};
use ubos_core::units::FanPwm;

use super::tests_harness::{FakeTach, LOOP_MUTEX, RecordingFan, Upstream, drive, moderate_reading};
use super::{CONTROL_INTERVAL, handle, run_cooling, stale_timeout};

/// Duty the operator picks first. Below the default 20% PID floor, which is
/// the point: the floor bounds PID output and must not clamp a fixed duty.
const FIRST_DUTY: u8 = 15;

/// Duty the operator switches to mid-run, and the case that proves a fixed 0
/// stops the fan rather than falling back to the floor.
const SECOND_DUTY: u8 = 0;

fn fixed_config(duty: u8) -> Config {
    let mut config = Config::default();
    config.cooling.mode = FanMode::Fixed;
    config.cooling.fixed_fan_pwm = duty;
    config
}

#[test]
fn fixed_mode_holds_the_duty_under_the_failsafes() {
    static UPSTREAM: Upstream = Upstream::new();
    // A poisoned lock only means an earlier test panicked while holding
    // it. This test publishes onto channels it owns, so it inherits no
    // stale state either way and `into_inner` just keeps testing.
    let _guard = LOOP_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let driver = MockDriver::get();
    driver.reset();

    let default_config = Config::default();
    let stale = stale_timeout(default_config.thermal.sample_interval_ms);
    let floor_pwm = FanPwm::from_percent(default_config.cooling.pid.min_fan_pwm);

    // The thermal watch retains whatever reading ran last in this process and
    // a fresh subscriber sees it as changed, so seed a known clean one rather
    // than inherit the previous test's fault reading. Both are published
    // before the task starts, so it comes up on the fixed mode as its initial
    // config rather than picking it up as a later change.
    UPSTREAM.publish_reading(moderate_reading(false));
    UPSTREAM.publish_config(fixed_config(FIRST_DUTY));

    let first_command = Cell::new(None);
    let mut cooling = pin!(run_cooling(
        RecordingFan(&first_command),
        FakeTach,
        UPSTREAM.thermal(),
        UPSTREAM.config()
    ));
    let mut ingest = pin!(ubos_metrics::metrics::run_ingest());

    // Behavior 1: the fan comes up on the configured duty before any reading
    // is acted on, so a board that boots in fixed mode never runs the fan at
    // the auto floor first. The duty is below that floor, so the boot branch
    // taking the auto arm would show here as 20% rather than 15%.
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        first_command.get(),
        Some(FanPwm::from_percent(FIRST_DUTY)),
        "fixed mode must start the fan on the configured duty, not the PID floor"
    );

    // Behavior 2: the fan settles on that duty. The seeded reading is well
    // below target, so PID control would land on the floor instead, and the
    // duty is below that floor, so neither the PID nor its clamp could have
    // produced this value.
    let settled_state = handle().current().expect("run_cooling publishes");
    assert_eq!(
        settled_state.pwm,
        FanPwm::from_percent(FIRST_DUTY),
        "fixed mode must command the configured duty, not the PID output"
    );

    // Behavior 3: a new duty reaches the fan on the next control tick, with
    // no thermal reading in between. Zero is commanded as zero: the PID floor
    // does not apply to a duty the operator chose.
    UPSTREAM.publish_config(fixed_config(SECOND_DUTY));
    driver.advance(CONTROL_INTERVAL);
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        FanPwm::from_percent(SECOND_DUTY),
        "a fixed duty change must reach the fan within one control tick"
    );

    // Behavior 4: a reading with no measurement still forces full. Fixed mode picks
    // the steady-state duty, it does not opt out of the failsafes.
    UPSTREAM.publish_reading(moderate_reading(true));
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        FanPwm::FULL,
        "a reading with no measurement must force the fan to full in fixed mode"
    );

    // Behavior 5: the control tick does not hand the fan back to the fixed
    // duty while the fault stands. Ticks interleave with the fault readings,
    // so a tick that reapplied the duty would cycle the fan.
    driver.advance(CONTROL_INTERVAL);
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        FanPwm::FULL,
        "a control tick must not lift the fault override in fixed mode"
    );

    // Behavior 6: a clean reading hands the fan back to the fixed duty.
    UPSTREAM.publish_reading(moderate_reading(false));
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        FanPwm::from_percent(SECOND_DUTY),
        "clearing the fault must return the fan to the fixed duty"
    );

    // Behavior 7: thermal silence past the staleness window forces full in
    // fixed mode too, so a wedged thermal task cannot leave a board that was
    // configured to a low duty uncooled.
    driver.advance(stale);
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        FanPwm::FULL,
        "thermal silence past stale_timeout must force full in fixed mode"
    );

    // Behavior 8: switching back to auto resumes PID control. The reading
    // that clears the staleness latch is well below target, so the PID lands
    // on the floor, which also shows the rebuilt controller is in charge
    // rather than the fixed duty.
    UPSTREAM.publish_config(default_config.clone());
    UPSTREAM.publish_reading(moderate_reading(false));
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        floor_pwm,
        "returning to auto must resume PID control"
    );

    // Behavior 9: the opposite transition. Every phase above entered fixed
    // mode as the task's initial config, so this is the one that exercises
    // refresh_control rebuilding the controller on the way in and the control
    // tick taking the fan off the PID. The task is on the floor from behavior
    // 8 and the duty is below it, so the reading the PID would act on cannot
    // produce this value.
    UPSTREAM.publish_config(fixed_config(FIRST_DUTY));
    driver.advance(CONTROL_INTERVAL);
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        FanPwm::from_percent(FIRST_DUTY),
        "entering fixed mode at runtime must take the fan off the PID"
    );

    // Behavior 10: leaving fixed mode raises the fan to the floor on the next
    // control tick rather than waiting for a thermal reading. The task is on
    // the duty from behavior 9, below the floor, and no reading is published
    // here, so the PID cannot be what produced this value. With a fixed 0 the
    // wait would be the difference between a stopped fan and a running one for
    // a whole thermal sample interval.
    //
    // The publish here is this phase's own input, not a cleanup: the config
    // watch is this test's own, so no other test reads what it leaves.
    UPSTREAM.publish_config(default_config);
    driver.advance(CONTROL_INTERVAL);
    drive(cooling.as_mut(), ingest.as_mut());
    assert_eq!(
        handle().current().expect("published").pwm,
        floor_pwm,
        "leaving fixed mode must raise the fan to the floor on the next tick"
    );
}
