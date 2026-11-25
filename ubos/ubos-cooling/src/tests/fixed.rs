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
//! phases, in a process of its own.

use core::cell::Cell;
use core::pin::pin;

use embassy_time::MockDriver;
use rusty_fork::rusty_fork_test;
use ubos_config::{Config, FanMode};
use ubos_core::units::FanPwm;

use super::tests_harness::{
    FakeTach, METRICS_STATICS, RecordingFan, Rig, default_config, drive, moderate_reading,
    publish_config, publish_reading, run_config,
};
use super::{CONTROL_INTERVAL, run_cooling, stale_timeout};

/// Duty the operator picks first. Below the default 20% PID floor, which is
/// the point: the floor bounds PID output and must not clamp a fixed duty.
const FIRST_DUTY: u8 = 15;

/// Duty the operator switches to mid-run, and the case that proves a fixed 0
/// stops the fan rather than falling back to the floor.
const SECOND_DUTY: u8 = 0;

fn fixed_config(duty: u8) -> Config {
    let mut config = default_config();
    config.cooling.mode = FanMode::Fixed;
    config.cooling.fixed_fan_pwm = duty;
    config
}

rusty_fork_test! {
    #[test]
    fn fixed_mode_holds_the_duty_under_the_failsafes() {
        let (rig, mut runners) = Rig::take();
        let driver = MockDriver::get();
        driver.reset();

        let mut config_task = pin!(run_config(&rig, &mut runners.config));
        drive!(config_task);

        let default_config = default_config();
        let stale = stale_timeout(default_config.thermal.sample_interval_ms);
        let floor_pwm = FanPwm::from_percent(default_config.cooling.pid.min_fan_pwm);

        // Both are published before the task starts, so it comes up on the
        // fixed mode as its initial config rather than as a later change.
        publish_config(&rig, fixed_config(FIRST_DUTY), || drive!(config_task));
        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(false), || drive!(config_task));

        let first_command = Cell::new(None);
        let mut cooling = pin!(run_cooling(
            RecordingFan(&first_command),
            FakeTach,
            &mut runners.fan,
            rig.thermal,
            rig.config
        ));
        let mut ingest = pin!(ubos_metrics::metrics::run_ingest(&METRICS_STATICS));

        // The duty is below the floor, so a boot that took the auto arm would
        // show here as the floor.
        drive!(config_task, cooling, ingest);
        assert_eq!(
            first_command.get(),
            Some(FanPwm::from_percent(FIRST_DUTY)),
            "fixed mode must start the fan on the configured duty, not the PID floor"
        );

        // The seeded reading is well below target and the duty is below the
        // floor, so neither the PID nor its clamp could produce this value.
        let settled_state = rig.fan.current().expect("run_cooling publishes");
        assert_eq!(
            settled_state.pwm,
            FanPwm::from_percent(FIRST_DUTY),
            "fixed mode must command the configured duty, not the PID output"
        );

        // No thermal reading arrives in between, so the control tick alone
        // applies the new duty.
        publish_config(&rig, fixed_config(SECOND_DUTY), || drive!(config_task, cooling, ingest));
        driver.advance(CONTROL_INTERVAL);
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            FanPwm::from_percent(SECOND_DUTY),
            "a fixed duty change must reach the fan within one control tick"
        );

        // Fixed mode picks the steady-state duty and keeps the failsafes.
        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(true), || drive!(config_task, cooling, ingest));
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            FanPwm::FULL,
            "a reading with no measurement must force the fan to full in fixed mode"
        );

        // Ticks interleave with the fault readings, so a tick that reapplied
        // the duty would cycle the fan.
        driver.advance(CONTROL_INTERVAL);
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            FanPwm::FULL,
            "a control tick must not lift the fault override in fixed mode"
        );

        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(false), || drive!(config_task, cooling, ingest));
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            FanPwm::from_percent(SECOND_DUTY),
            "clearing the fault must return the fan to the fixed duty"
        );

        // A wedged thermal task must not leave a board on a low fixed duty
        // uncooled.
        driver.advance(stale);
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            FanPwm::FULL,
            "thermal silence past stale_timeout must force full in fixed mode"
        );

        // The reading that clears the staleness latch is well below target, so
        // the floor shows the rebuilt controller is in charge, not the fixed duty.
        publish_config(&rig, default_config.clone(), || drive!(config_task, cooling, ingest));
        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(false), || drive!(config_task, cooling, ingest));
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            floor_pwm,
            "returning to auto must resume PID control"
        );

        // Every phase above entered fixed mode as the initial config, so this
        // one exercises `refresh_control` and the tick taking the fan off the
        // PID. The fan is on the floor and the duty is below it, so the PID
        // cannot produce this value.
        publish_config(&rig, fixed_config(FIRST_DUTY), || drive!(config_task, cooling, ingest));
        driver.advance(CONTROL_INTERVAL);
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            FanPwm::from_percent(FIRST_DUTY),
            "entering fixed mode at runtime must take the fan off the PID"
        );

        // No reading is published here and the fan is below the floor, so only
        // the control tick can raise it. With a fixed 0, waiting for a reading
        // would leave the fan stopped for a whole sample interval.
        publish_config(&rig, default_config, || drive!(config_task, cooling, ingest));
        driver.advance(CONTROL_INTERVAL);
        drive!(config_task, cooling, ingest);
        assert_eq!(
            rig.fan.current().expect("published").pwm,
            floor_pwm,
            "leaving fixed mode must raise the fan to the floor on the next tick"
        );
    }
}
