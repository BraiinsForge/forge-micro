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
//! clock, a sensor or a manager.
//!
//! `judge` reads the halts from the mask it is handed and sends nothing,
//! so each test holds a mask of its own and applies to it the message
//! each note the step carries stands for, as the manager would.

extern crate std;

use ubos_config::TemperatureDescriptor;
use ubos_core::units::Temperature;
use ubos_manager::{Message, PauseReasons};

use super::{Note, Reading, SENSOR_FAULT_TICKS, Step, TempSample, TempStats, Thermal};

/// A sample of one hashboard reading.
fn hashboard(temp: Temperature) -> TempSample {
    TempSample {
        chip: None,
        hashboard: Some(TempStats {
            max: temp,
            mean: temp,
        }),
    }
}

/// A measurement well below every threshold, so a tick on it crosses no
/// edge of the overheat pair.
fn cool_reading() -> Reading {
    Reading::Measured(hashboard(Temperature::from_millicelsius(50_000)))
}

/// Judge one reading against `held`, and answer what the tick decided.
fn judge(thermal: &mut Thermal, reading: Reading, held: PauseReasons) -> Step {
    let config = crate::default_config().thermal;
    thermal.judge(reading, &config, held)
}

/// The step of a tick that crossed no edge.
fn quiet() -> Step {
    Step {
        unreadable: None,
        dangerous: None,
    }
}

/// Run the budget out, as the manager would see it: the mask holds the
/// unreadable halt afterwards.
fn spend_budget(thermal: &mut Thermal) -> PauseReasons {
    let mut held = PauseReasons::default();
    for _ in 0..SENSOR_FAULT_TICKS {
        let step = judge(thermal, Reading::Unreadable, held);
        if step.unreadable.is_some() {
            held = held.apply(Message::TemperatureUnreadable);
        }
    }
    assert!(held.contains(ubos_manager::PauseReason::TemperatureUnreadable));
    held
}

#[test]
fn a_cool_measurement_crosses_no_edge() {
    let mut thermal = Thermal::default();

    let step = judge(&mut thermal, cool_reading(), PauseReasons::default());

    assert_eq!(step, quiet());
}

#[test]
fn silence_asks_for_the_halt_once_when_the_budget_is_spent() {
    let mut thermal = Thermal::default();
    judge(&mut thermal, cool_reading(), PauseReasons::default());

    for _ in 1..SENSOR_FAULT_TICKS {
        let step = judge(&mut thermal, Reading::Unreadable, PauseReasons::default());
        assert_eq!(step, quiet());
    }

    // The tick that spends the budget asks for the halt.
    let step = judge(&mut thermal, Reading::Unreadable, PauseReasons::default());
    assert_eq!(
        step,
        Step {
            unreadable: Some(Note::Unreadable(SENSOR_FAULT_TICKS)),
            ..quiet()
        }
    );

    // Past the budget no tick asks again, because the mask already
    // holds the halt.
    let held_mask = PauseReasons::default().apply(Message::TemperatureUnreadable);
    assert_eq!(judge(&mut thermal, Reading::Unreadable, held_mask), quiet());
}

#[test]
fn an_idle_board_releases_the_halt_it_asked_for() {
    let mut thermal = Thermal::default();
    let held_mask = spend_budget(&mut thermal);

    assert_eq!(
        judge(&mut thermal, Reading::Idle, held_mask),
        Step {
            unreadable: Some(Note::Idle),
            ..quiet()
        }
    );

    // An idle tick with no halt to release asks for nothing.
    let released = held_mask.apply(Message::TemperatureReadable);
    assert_eq!(judge(&mut thermal, Reading::Idle, released), quiet());
}

#[test]
fn a_measurement_releases_the_halt_and_the_budget_starts_over() {
    let mut thermal = Thermal::default();
    let held_mask = spend_budget(&mut thermal);

    assert_eq!(
        judge(&mut thermal, cool_reading(), held_mask),
        Step {
            unreadable: Some(Note::Recovered(SENSOR_FAULT_TICKS)),
            ..quiet()
        }
    );

    // The next silence is ridden out again: the release reset the count.
    let released = held_mask.apply(Message::TemperatureReadable);
    assert_eq!(judge(&mut thermal, Reading::Unreadable, released), quiet());
}

#[test]
fn a_dangerous_measurement_halts_once_and_a_cool_one_under_the_halt_resumes() {
    let config = crate::default_config().thermal;
    // One degree above the hashboard threshold the default config sets.
    let threshold = config
        .dangerous
        .hashboard
        .expect("BUG: the default machine reads no hashboard");
    let hot = Temperature::from_millicelsius(threshold.millicelsius() + 1000);
    let mut thermal = Thermal::default();

    let step = judge(
        &mut thermal,
        Reading::Measured(hashboard(hot)),
        PauseReasons::default(),
    );
    assert_eq!(step.dangerous, Some(Note::Dangerous(hot)));

    // Under the halt the same reading crosses nothing.
    let halted = PauseReasons::default().apply(Message::DangerousTemperature);
    let step = judge(&mut thermal, Reading::Measured(hashboard(hot)), halted);
    assert_eq!(step.dangerous, None);

    // The cool reading sits above the cooldown threshold, so the halt
    // stands on it; one degree below that threshold releases it.
    let step = judge(&mut thermal, cool_reading(), halted);
    assert_eq!(step.dangerous, None);
    let cooled = Temperature::from_millicelsius(config.cooldown_temp.millicelsius() - 1000);
    let step = judge(&mut thermal, Reading::Measured(hashboard(cooled)), halted);
    assert_eq!(step.dangerous, Some(Note::Cooled(cooled)));
}

#[test]
fn every_kind_of_a_sample_is_judged_against_its_own_threshold() {
    let mut config = crate::default_config().thermal;
    let (dangerous_chip, dangerous_hashboard) = (
        Temperature::from_celsius(110),
        Temperature::from_celsius(83),
    );
    config.dangerous = TemperatureDescriptor {
        chip: Some(dangerous_chip),
        hashboard: Some(dangerous_hashboard),
    };
    // The hashboard runs past its own threshold while the die is still
    // below the die one, and either kind at its own threshold halts.
    let hot = Temperature::from_millicelsius(dangerous_hashboard.millicelsius() + 1000);
    let die = Temperature::from_millicelsius(dangerous_chip.millicelsius() - 1000);
    let cool = Temperature::from_millicelsius(config.cooldown_temp.millicelsius() - 1000);
    let both = |chip: Temperature, hashboard: Temperature| {
        Reading::Measured(TempSample {
            chip: Some(TempStats {
                max: chip,
                mean: chip,
            }),
            hashboard: Some(TempStats {
                max: hashboard,
                mean: hashboard,
            }),
        })
    };
    let mut thermal = Thermal::default();

    let step = thermal.judge(both(die, hot), &config, PauseReasons::default());
    assert_eq!(step.dangerous, Some(Note::Dangerous(hot)));

    // Under the halt a cool hashboard resumes nothing while the die is
    // still warm: every kind read has to cool.
    let halted = PauseReasons::default().apply(Message::DangerousTemperature);
    let step = thermal.judge(both(die, cool), &config, halted);
    assert_eq!(step.dangerous, None);
    let step = thermal.judge(both(cool, cool), &config, halted);
    assert_eq!(step.dangerous, Some(Note::Cooled(cool)));
}
