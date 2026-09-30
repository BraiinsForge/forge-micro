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

//! Unit tests for [`super::Thermal::judge`], the decision one tick makes
//! from the sensor's answer, driven directly and without the loop, the
//! clock or a sensor.
//!
//! Each test stands in for the manager with channels of its own, as the
//! `run_thermal` tests do, and applies what `judge` sent to the mask it
//! publishes, so the read-back `judge` does on the next call sees it.

extern crate std;

use ubos_core::units::Temperature;
use ubos_manager::{Message, PauseReasons};

use super::tests_run_thermal::{ManagerChannels, drain_messages};
use super::{Reading, SENSOR_FAULT_TICKS, Sample, Thermal, Tick};

/// A measurement well below every threshold, so the only thing a tick
/// on it decides is what to publish.
fn cool_reading() -> Reading {
    Reading::Measured(Sample::sole(Temperature::from_millicelsius(50_000)))
}

/// Judge one reading and answer what the tick decided.
fn judge(thermal: &mut Thermal, reading: Reading, manager: &'static ManagerChannels) -> Tick {
    let config = ubos_config::Config::default().thermal;
    embassy_futures::block_on(thermal.judge(reading, &config, &manager.handle()))
}

#[test]
fn a_measurement_publishes_this_ticks_pid_temperature() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let mut mask = PauseReasons::default();
    drain_messages(&MANAGER, &mut mask);
    let mut thermal = Thermal::default();

    let tick = judge(&mut thermal, cool_reading(), &MANAGER);

    assert_eq!(
        tick,
        Tick::Publish {
            chip_max: Some(Temperature::from_millicelsius(50_000)),
            pid_temp: Some(Temperature::from_millicelsius(50_000)),
        }
    );
    assert_eq!(
        thermal.last_chip_max,
        Some(Temperature::from_millicelsius(50_000))
    );
    assert!(drain_messages(&MANAGER, &mut mask).is_empty());
}

#[test]
fn silence_is_withheld_inside_the_budget_and_published_held_past_it() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let mut mask = PauseReasons::default();
    drain_messages(&MANAGER, &mut mask);
    let mut thermal = Thermal::default();
    judge(&mut thermal, cool_reading(), &MANAGER);
    drain_messages(&MANAGER, &mut mask);

    for _ in 1..SENSOR_FAULT_TICKS {
        assert_eq!(
            judge(&mut thermal, Reading::Unreadable, &MANAGER),
            Tick::Withhold
        );
        assert!(drain_messages(&MANAGER, &mut mask).is_empty());
    }

    // The tick that spends the budget halts, and publishes the held
    // chip temperature with no PID temperature beside it.
    assert_eq!(
        judge(&mut thermal, Reading::Unreadable, &MANAGER),
        Tick::Publish {
            chip_max: Some(Temperature::from_millicelsius(50_000)),
            pid_temp: None,
        }
    );
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::TemperatureUnreadable]
    );

    // Past the budget every tick publishes and none asks again, because
    // the mask already holds the halt.
    assert_eq!(
        judge(&mut thermal, Reading::Unreadable, &MANAGER),
        Tick::Publish {
            chip_max: Some(Temperature::from_millicelsius(50_000)),
            pid_temp: None,
        }
    );
    assert!(drain_messages(&MANAGER, &mut mask).is_empty());
}

#[test]
fn an_idle_board_publishes_held_and_releases_the_halt_it_asked_for() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let mut mask = PauseReasons::default();
    drain_messages(&MANAGER, &mut mask);
    let mut thermal = Thermal::default();
    for _ in 0..SENSOR_FAULT_TICKS {
        judge(&mut thermal, Reading::Unreadable, &MANAGER);
    }
    drain_messages(&MANAGER, &mut mask);

    assert_eq!(
        judge(&mut thermal, Reading::Idle, &MANAGER),
        Tick::Publish {
            chip_max: None,
            pid_temp: None,
        }
    );
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::TemperatureReadable]
    );

    // An idle tick with no halt to release still publishes, and asks
    // for nothing.
    assert_eq!(
        judge(&mut thermal, Reading::Idle, &MANAGER),
        Tick::Publish {
            chip_max: None,
            pid_temp: None,
        }
    );
    assert!(drain_messages(&MANAGER, &mut mask).is_empty());
}

#[test]
fn a_measurement_releases_the_halt_and_the_budget_starts_over() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let mut mask = PauseReasons::default();
    drain_messages(&MANAGER, &mut mask);
    let mut thermal = Thermal::default();
    for _ in 0..SENSOR_FAULT_TICKS {
        judge(&mut thermal, Reading::Unreadable, &MANAGER);
    }
    drain_messages(&MANAGER, &mut mask);

    assert_eq!(
        judge(&mut thermal, cool_reading(), &MANAGER),
        Tick::Publish {
            chip_max: Some(Temperature::from_millicelsius(50_000)),
            pid_temp: Some(Temperature::from_millicelsius(50_000)),
        }
    );
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::TemperatureReadable]
    );

    // The next silence is withheld again: the release reset the count.
    assert_eq!(
        judge(&mut thermal, Reading::Unreadable, &MANAGER),
        Tick::Withhold
    );
    assert!(drain_messages(&MANAGER, &mut mask).is_empty());
}
