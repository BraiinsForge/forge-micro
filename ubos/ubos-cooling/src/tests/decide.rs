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
use ubos_config::{PidTarget, Source, Statistic};
use ubos_core::units::{FanPwm, Temperature};
use ubos_thermal::{TempSample, TempStats};

use super::tests_harness::default_config;
use super::{Cooling, Hold, Note, PidInput, Step, stale_timeout};

/// Well below the default target, so the PID drives the duty toward its
/// floor rather than toward full.
const COOL: Temperature = Temperature::from_millicelsius(50_000);

/// The moment the tests start their clock at.
const START: Instant = Instant::from_millis(1_000);

/// A sample of hashboard readings whose hottest and mean are both
/// `temp`, the one kind the default board reads.
fn hashboard(temp: Temperature) -> TempSample {
    TempSample {
        chip: None,
        hashboard: Some(TempStats {
            max: temp,
            mean: temp,
        }),
    }
}

/// A step that reports `pwm` and no transition.
fn quiet(pwm: FanPwm) -> Step {
    Step { pwm, note: None }
}

fn floor(config: &Config) -> FanPwm {
    FanPwm::from_percent(config.cooling.pid.min_fan_pwm)
}

/// A config with the fan taken off the PID at `duty` percent.
fn fixed(duty: u8) -> Config {
    let mut config = default_config();
    config.cooling.mode = FanMode::Fixed;
    config.cooling.fixed_fan_pwm = duty;
    config
}

#[test]
fn auto_starts_at_the_floor_and_a_measurement_hands_the_fan_to_the_pid() {
    let config = default_config();
    let mut cooling = Cooling::new(&config, START);
    assert_eq!(cooling.pwm, floor(&config));
    assert_eq!(cooling.hold, Hold::None);

    // A control tick before any reading keeps the floor.
    assert_eq!(cooling.on_tick(START), quiet(floor(&config)));

    // A cool measurement leaves the PID at its floor, and the fan is not
    // held.
    let step = cooling.on_reading(Some(hashboard(COOL)), START);
    assert_eq!(step, quiet(floor(&config)));
    assert_eq!(cooling.hold, Hold::None);
}

#[test]
fn a_reading_with_no_measurement_parks_the_fan_until_one_arrives() {
    let config = default_config();
    let mut cooling = Cooling::new(&config, START);

    assert_eq!(cooling.on_reading(None, START), quiet(FanPwm::FULL));
    assert_eq!(cooling.hold, Hold::NoMeasurement);

    // The control tick in between two such readings must not hand the
    // floor back to the fan.
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::FULL));

    // The next measurement releases the hold.
    assert_eq!(
        cooling.on_reading(Some(hashboard(COOL)), START),
        quiet(floor(&config))
    );
    assert_eq!(cooling.hold, Hold::None);
}

#[test]
fn a_measurement_after_a_hold_reaches_the_pid_unsmoothed() {
    // A proportional-only PID whose duty reads back the temperature it
    // saw, behind a smoother that would otherwise blend 80 into 40.
    let mut config = default_config();
    config.cooling.target.temp = Temperature::from_millicelsius(0);
    config.cooling.pid.p_gain = -1.0;
    config.cooling.pid.i_gain = 0.0;
    config.cooling.pid.d_gain = 0.0;
    config.cooling.pid.min_fan_pwm = 0;
    config.cooling.pid.input_smoother_half_decay_secs = 10;
    let mut cooling = Cooling::new(&config, START);

    let celsius = |c: i32| Some(hashboard(Temperature::from_millicelsius(c * 1000)));
    assert_eq!(
        cooling.on_reading(celsius(40), START),
        quiet(FanPwm::from_percent(40))
    );
    assert_eq!(cooling.on_reading(None, START), quiet(FanPwm::FULL));
    assert_eq!(
        cooling.on_reading(celsius(80), START),
        quiet(FanPwm::from_percent(80))
    );
}

#[test]
fn a_stall_forces_full_once_and_a_reading_recovers_it_once() {
    let config = default_config();
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
    assert_eq!(
        cooling.on_reading(Some(hashboard(COOL)), START),
        quiet(duty)
    );
    assert_eq!(cooling.on_tick(START), quiet(duty));

    // A reading with no measurement parks the fan in fixed mode too, and
    // the tick does not hand the fixed duty back.
    assert_eq!(cooling.on_reading(None, START), quiet(FanPwm::FULL));
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::FULL));
    assert_eq!(
        cooling.on_reading(Some(hashboard(COOL)), START),
        quiet(duty)
    );

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
    let auto = default_config();
    // A fixed duty below the floor, which the PID would not command.
    let mut cooling = Cooling::new(&fixed(0), START);
    assert_eq!(cooling.on_tick(START), quiet(FanPwm::from_percent(0)));

    cooling.reconfigure(&auto);
    // The PID commands nothing until the next reading, so the tick is
    // what lifts the fan off the fixed duty.
    assert_eq!(cooling.on_tick(START), quiet(floor(&auto)));
}

#[test]
fn the_pid_follows_the_hashboard_max_and_never_the_die() {
    let config = default_config();
    let mut cooling = Cooling::new(&config, START);
    let hot = Temperature::from_celsius(90);
    let chip = TempStats {
        max: hot,
        mean: COOL,
    };

    // The hashboard maximum, even beside a hotter die.
    cooling.on_reading(
        Some(TempSample {
            chip: Some(chip),
            hashboard: Some(TempStats {
                max: COOL,
                mean: COOL,
            }),
        }),
        START,
    );
    assert_eq!(
        cooling.pid_input(),
        Some(PidInput {
            source: Source::Hashboard,
            statistic: Statistic::Max,
            temp: COOL,
            target: config.cooling.target.temp,
        })
    );

    // A sample without the hashboard does not hand the PID the die: it
    // measured nothing the PID follows, so the fan runs at full.
    assert_eq!(
        cooling
            .on_reading(
                Some(TempSample {
                    chip: Some(chip),
                    hashboard: None,
                }),
                START,
            )
            .pwm,
        FanPwm::FULL
    );
    assert_eq!(cooling.pid_input(), None);
}

#[test]
fn the_pid_follows_nothing_under_a_hold_or_a_fixed_duty() {
    let mut cooling = Cooling::new(&default_config(), START);
    cooling.on_reading(Some(hashboard(COOL)), START);
    assert!(cooling.pid_input().is_some());
    cooling.on_reading(None, START);
    assert_eq!(cooling.pid_input(), None);

    let mut cooling = Cooling::new(&fixed(30), START);
    cooling.on_reading(Some(hashboard(COOL)), START);
    assert_eq!(cooling.pid_input(), None);
}

#[test]
fn the_pid_follows_only_the_target_kind_and_without_it_runs_full() {
    let mut config = default_config();
    config.cooling.target = PidTarget {
        source: Source::Chip,
        statistic: Statistic::Mean,
        temp: config.cooling.target.temp,
    };
    let mut cooling = Cooling::new(&config, START);
    let warm = Temperature::from_celsius(60);
    let hot = Temperature::from_celsius(90);
    let both = TempSample {
        chip: Some(TempStats {
            max: hot,
            mean: warm,
        }),
        hashboard: Some(TempStats {
            max: COOL,
            mean: COOL,
        }),
    };
    let input = |cooling: &Cooling| {
        cooling
            .pid_input()
            .map(|input| (input.source, input.statistic, input.temp))
    };

    // The chip mean, though the hottest die and the hashboard differ.
    cooling.on_reading(Some(both), START);
    assert_eq!(input(&cooling), Some((Source::Chip, Statistic::Mean, warm)));

    // Without a chip reading the hashboard is never followed, and the fan
    // runs at full at once, as on a reading that measured nothing.
    assert_eq!(
        cooling.on_reading(Some(hashboard(hot)), START).pwm,
        FanPwm::FULL
    );
    assert_eq!(cooling.pid_input(), None);

    // Once the chips read again, the PID follows them.
    cooling.on_reading(Some(both), START);
    assert_eq!(input(&cooling), Some((Source::Chip, Statistic::Mean, warm)));
}

#[test]
fn a_new_target_kind_reaches_the_pid_unsmoothed() {
    // The proportional-only PID of the test above for a hold, whose duty
    // reads back the temperature it saw.
    let mut config = default_config();
    config.cooling.target.temp = Temperature::from_millicelsius(0);
    config.cooling.pid.p_gain = -1.0;
    config.cooling.pid.i_gain = 0.0;
    config.cooling.pid.d_gain = 0.0;
    config.cooling.pid.min_fan_pwm = 0;
    config.cooling.pid.input_smoother_half_decay_secs = 10;
    let mut cooling = Cooling::new(&config, START);
    let celsius = |c: i32| Temperature::from_millicelsius(c * 1000);
    let both = Some(TempSample {
        chip: Some(TempStats {
            max: celsius(80),
            mean: celsius(80),
        }),
        hashboard: Some(TempStats {
            max: celsius(40),
            mean: celsius(40),
        }),
    });
    assert_eq!(
        cooling.on_reading(both, START),
        quiet(FanPwm::from_percent(40))
    );

    // The die reading reaches the PID whole, not blended into the
    // hashboard reading the smoother held.
    config.cooling.target.source = Source::Chip;
    cooling.reconfigure(&config);
    assert_eq!(
        cooling.on_reading(both, START),
        quiet(FanPwm::from_percent(80))
    );
}

#[test]
fn a_fixed_duty_ignores_which_kinds_a_sample_read() {
    let mut cooling = Cooling::new(&fixed(30), START);
    // The default target follows the hashboard, and this sample read only
    // the chips: the PID would have nothing to follow, the duty holds.
    let chips_only = TempSample {
        chip: Some(TempStats {
            max: COOL,
            mean: COOL,
        }),
        hashboard: None,
    };
    assert_eq!(
        cooling.on_reading(Some(chips_only), START),
        quiet(FanPwm::from_percent(30))
    );

    // A reading that measured nothing still holds the fan at full.
    assert_eq!(cooling.on_reading(None, START).pwm, FanPwm::FULL);
}
