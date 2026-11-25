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

//! Coverage for the name check: [`Name::new`] accepts a name of each
//! unit and refuses each rule in turn. The tests call the `const fn` at
//! run time, where a refused name is a panic that a test can expect.

use ubos_core::units::{Current, FanPwm, FanRpm, Frequency, Power, Temperature, Voltage};

use super::Name;
use crate::value::{Count, Flag, Ratio};

/// Every unit accepts a name that ends in its suffix, and a unit with no
/// suffix accepts a name that states none.
#[test]
fn a_name_of_each_unit_is_accepted() {
    assert_eq!(
        Name::<Temperature>::new("mcu_temperature_celsius").as_str(),
        "mcu_temperature_celsius"
    );
    let _ = Name::<Voltage>::new("psu_set_voltage_volts");
    let _ = Name::<Power>::new("psu_power_watts");
    let _ = Name::<Current>::new("psu_current_amperes");
    let _ = Name::<Frequency>::new("frequency_hertz");
    let _ = Name::<Ratio>::new("cpu_busy_ratio");
    let _ = Name::<FanPwm>::new("fan_speed_control");
    let _ = Name::<FanRpm>::new("fan_rpm");
    let _ = Name::<Flag>::new("miner_state");
    let _ = Name::<Count>::new("pool_reconnects");
}

/// A count may state seconds, because no unit type holds a duration.
#[test]
fn a_count_in_seconds_is_accepted() {
    let _ = Name::<Count>::new("uptime_seconds");
}

/// The check leaves the `_total` ending to review.
#[test]
fn a_name_that_ends_in_total_is_accepted() {
    let _ = Name::<Count>::new("metrics_dropped_total");
}

/// The two names whose noun is the hashboard keep the prefix.
#[test]
fn a_listed_hashboard_name_is_accepted() {
    let _ = Name::<Flag>::new("hashboard_present");
    let _ = Name::<Count>::new("hashboard_shares");
}

#[test]
#[should_panic(expected = "the name is empty")]
fn an_empty_name_is_refused() {
    let _ = Name::<Count>::new("");
}

#[test]
#[should_panic(expected = "the name does not start with a lowercase letter")]
fn a_leading_digit_is_refused() {
    let _ = Name::<Count>::new("3v3_rail");
}

#[test]
#[should_panic(expected = "the name has a character that is not")]
fn a_capital_letter_is_refused() {
    let _ = Name::<Count>::new("miner_State");
}

#[test]
#[should_panic(expected = "the name has two underscores in a row")]
fn a_double_underscore_is_refused() {
    let _ = Name::<Count>::new("miner__state");
}

#[test]
#[should_panic(expected = "the name ends in an underscore")]
fn a_trailing_underscore_is_refused() {
    let _ = Name::<Count>::new("miner_state_");
}

#[test]
#[should_panic(expected = "the name starts with the prefix board_")]
fn the_board_prefix_is_refused() {
    let _ = Name::<Count>::new("board_x");
}

#[test]
#[should_panic(expected = "the name starts with the prefix hashboard_")]
fn an_unlisted_hashboard_prefix_is_refused() {
    let _ = Name::<Count>::new("hashboard_x");
}

#[test]
#[should_panic(expected = "the name ends in a unit that is not a base unit")]
fn a_scaled_suffix_is_refused() {
    let _ = Name::<Count>::new("x_mv");
}

#[test]
#[should_panic(expected = "the name ends in a unit that is not a base unit")]
fn a_percentage_is_refused() {
    let _ = Name::<FanPwm>::new("x_percent");
}

#[test]
#[should_panic(expected = "the name ends in a unit that is not a base unit")]
fn a_scale_word_is_refused() {
    let _ = Name::<Count>::new("x_millicelsius");
}

#[test]
#[should_panic(expected = "the name ends in a unit that does not fit the value")]
fn a_count_that_states_a_unit_is_refused() {
    let _ = Name::<Count>::new("x_watts");
}

#[test]
#[should_panic(expected = "the name does not end in the unit of the value")]
fn a_voltage_that_states_another_unit_is_refused() {
    let _ = Name::<Voltage>::new("x_celsius");
}
