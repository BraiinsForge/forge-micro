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
//! All phases share one `run_cooling` instance and the process-global
//! statics it reads and writes: the mock clock, the handles, and the
//! metrics channel. The test runs as a single function with sequential
//! phases, and in a process of its own.
//!
//! The "log once" behavior on the stall and recovery transitions is the
//! `Note` that `Cooling` attaches to a `Hold::Stalled` transition, and
//! is not asserted here: this test runs under `--no-default-features`,
//! with defmt off and no log sink to count emissions against.

use core::cell::Cell;
use core::pin::pin;

use embassy_time::MockDriver;
use rusty_fork::rusty_fork_test;
use ubos_core::units::FanPwm;

use super::tests_harness::{
    FakeTach, METRICS_STATICS, RecordingFan, Rig, default_config, drive, moderate_reading,
    publish_reading, run_config,
};
use super::{CONTROL_INTERVAL, run_cooling, stale_timeout};

rusty_fork_test! {
    #[test]
    fn staleness_failsafe_forces_and_releases_full_fan() {
        let (rig, mut runners) = Rig::take();
        let driver = MockDriver::get();
        driver.reset();

        // `run_thermal` and `run_cooling` wait for the config that this task
        // publishes on its first poll.
        let mut config_task = pin!(run_config(&rig, &mut runners.config));
        drive!(config_task);

        let default_config = default_config();
        let stale = stale_timeout(default_config.thermal.sample_interval_ms);
        let floor_pwm = FanPwm::from_percent(default_config.cooling.pid.min_fan_pwm);

        // The thermal watch holds the last reading and a fresh subscriber
        // sees it as changed, so seed a known clean one before the task
        // starts.
        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(false), || {});

        let boot_command = Cell::new(None);
        let mut cooling = pin!(run_cooling(
            RecordingFan(&boot_command),
            FakeTach,
            &mut runners.fan,
            rig.thermal,
            rig.config
        ));
        let mut ingest = pin!(ubos_metrics::metrics::run_ingest(&METRICS_STATICS));

        drive!(cooling, ingest);
        // Asserted on the first command, not the watch: see `RecordingFan`.
        assert_eq!(
            boot_command.get(),
            Some(floor_pwm),
            "auto mode must start the fan on the PID floor, not the fixed duty"
        );

        let settled_state = rig.fan.current().expect("run_cooling publishes");
        assert_eq!(
            settled_state.pwm, floor_pwm,
            "the seeded reading is well below target, so PID control settles the \
             fan on the min_fan_pwm floor"
        );

        driver.advance(stale);
        drive!(cooling, ingest);
        let stalled_state = rig.fan.current().expect("published");
        assert_eq!(
            stalled_state.pwm,
            FanPwm::FULL,
            "thermal silence past stale_timeout must force the fan to full"
        );

        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(false), || drive!(cooling, ingest));
        let recovered_state = rig.fan.current().expect("published");
        assert_eq!(
            recovered_state.pwm, floor_pwm,
            "a fresh reading well below target must clear the staleness latch and \
             resume PID control, which drives the PWM down to the min_fan_pwm floor"
        );

        // Pins the `silence >= stale` boundary from below, so the failsafe
        // cannot fire one tick early.
        let short_silence = stale - CONTROL_INTERVAL;
        driver.advance(short_silence);
        drive!(cooling, ingest);
        let short_silence_state = rig.fan.current().expect("published");
        assert_eq!(
            short_silence_state.pwm, floor_pwm,
            "silence one tick short of stale_timeout must not force the fan to full; \
             PID control still holds the PWM at the min_fan_pwm floor"
        );

        // The staleness latch is clear here, so the fault reading alone
        // forces full.
        publish_reading(&rig, &mut runners.thermal, driver, moderate_reading(true), || drive!(cooling, ingest));
        let fault_state = rig.fan.current().expect("published");
        assert_eq!(
            fault_state.pwm,
            FanPwm::FULL,
            "a reading with no measurement must force the fan to full"
        );
    }
}
