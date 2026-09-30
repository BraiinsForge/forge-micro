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

//! Unit tests for [`super::Cooling`], the fan decision, driven directly
//! with a clock of their own: no fan, no tach, no thermal task and no
//! executor. Each test walks the holds one transition at a time and
//! asserts the duty and the note each step reports.

use embassy_time::{Duration, Instant};
use ubos_config::{Config, FanMode};
use ubos_core::units::{FanPwm, Temperature};

use super::{Cooling, Hold, Note, Step, stale_timeout};

/// Well below the default target, so the PID drives the duty toward its
/// floor rather than toward full.
const COOL: Temperature = Temperature::from_millicelsius(50_000);

/// The moment the tests start their clock at.
const START: Instant = Instant::from_millis(1_000);

/// A step that reports `pwm` and no transition.
fn quiet(pwm: FanPwm) -> Step {
    Step { pwm, note: None }
}

fn floor(config: &Config) -> FanPwm {
    FanPwm::from_percent(config.cooling.pid.min_fan_pwm)
}

/// A config with the fan taken off the PID at `duty` percent.
fn fixed(duty: u8) -> Config {
    let mut config = Config::default();
    config.cooling.mode = FanMode::Fixed;
    config.cooling.fixed_fan_pwm = duty;
    config
}

#[test]
fn auto_starts_at_the_floor_and_a_measurement_hands_the_fan_to_the_pid() {
    let config = Config::default();
    let mut cooling = Cooling::new(&config, START);
    assert_eq!(cooling.pwm, floor(&config));
    assert_eq!(cooling.hold, Hold::None);

    // A control tick before any reading keeps the floor.
    assert_eq!(cooling.on_tick(START), quiet(floor(&config)));

    // A cool measurement leaves the PID at its floor, and the fan is not
    // held.
    let step = cooling.on_reading(Some(COOL), START);
    assert_eq!(step, quiet(floor(&config)));
    assert_eq!(cooling.hold, Hold::None);
}

#[test]
fn a_reading_with_no_measurement_parks_the_fan_until_one_arrives() {
    let config = Config::default();
    let mut cooling = Cooling::new(&config, START);

    assert_eq!(cooling.on_reading(None, START), quiet(FanPwm::FULL));
    assert_eq!(cooling.hold, Hold::NoMeasurement);

    // The control tick in between two such readings must not hand the
    // floor back to the fan.
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::FULL));

    // The next measurement releases the hold.
    assert_eq!(cooling.on_reading(Some(COOL), START), quiet(floor(&config)));
    assert_eq!(cooling.hold, Hold::None);
}

#[test]
fn a_stall_forces_full_once_and_a_reading_recovers_it_once() {
    let config = Config::default();
    let stale = stale_timeout(config.thermal.sample_interval_ms);
    let mut cooling = Cooling::new(&config, START);

    // Inside the window nothing happens.
    let inside = START + stale - Duration::from_millis(1);
    assert_eq!(cooling.on_tick(inside), quiet(floor(&config)));
    assert_eq!(cooling.hold, Hold::None);

    // The tick that finds the window elapsed stalls, and says so once.
    let stalled = START + stale;
    assert_eq!(
        cooling.on_tick(stalled),
        Step {
            pwm: FanPwm::FULL,
            note: Some(Note::Stalled(stale)),
        }
    );
    assert_eq!(cooling.hold, Hold::Stalled);
    assert_eq!(cooling.on_tick(stalled + stale), quiet(FanPwm::FULL));

    // A reading without a measurement still clears the stall, because the
    // task is alive to have sent it; the fan stays at full for it.
    let back = stalled + stale;
    assert_eq!(
        cooling.on_reading(None, back),
        Step {
            pwm: FanPwm::FULL,
            note: Some(Note::Recovered),
        }
    );
    assert_eq!(cooling.hold, Hold::NoMeasurement);

    // The stall clock restarted with that reading.
    assert_eq!(
        cooling.on_tick(back + stale - Duration::from_millis(1)),
        quiet(FanPwm::FULL)
    );
    assert_eq!(cooling.hold, Hold::NoMeasurement);
}

#[test]
fn fixed_mode_holds_the_duty_and_the_failsafes_still_apply() {
    let config = fixed(30);
    let duty = FanPwm::from_percent(30);
    let stale = stale_timeout(config.thermal.sample_interval_ms);
    let mut cooling = Cooling::new(&config, START);
    assert_eq!(cooling.pwm, duty);

    // A measurement does not move the duty and the tick carries it.
    assert_eq!(cooling.on_reading(Some(COOL), START), quiet(duty));
    assert_eq!(cooling.on_tick(START), quiet(duty));

    // A reading with no measurement parks the fan in fixed mode too, and
    // the tick does not hand the fixed duty back.
    assert_eq!(cooling.on_reading(None, START), quiet(FanPwm::FULL));
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::FULL));
    assert_eq!(cooling.on_reading(Some(COOL), START), quiet(duty));

    // So does the stall.
    let step = cooling.on_tick(START + stale);
    assert_eq!(step.pwm, FanPwm::FULL);
    assert_eq!(step.note, Some(Note::Stalled(stale)));
}

#[test]
fn a_fixed_duty_change_lands_on_the_next_tick() {
    let mut cooling = Cooling::new(&fixed(30), START);
    cooling.reconfigure(&fixed(60));
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::from_percent(60)));
}

#[test]
fn leaving_fixed_mode_raises_the_fan_to_the_floor_on_the_next_tick() {
    let auto = Config::default();
    // A fixed duty below the floor, which the PID would not command.
    let mut cooling = Cooling::new(&fixed(0), START);
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::from_percent(0)));

    cooling.reconfigure(&auto);
    // The PID commands nothing until the next reading, so the tick is
    // what lifts the fan off the fixed duty.
    assert_eq!(cooling.on_tick(START), quiet(floor(&auto)));
}
