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

//! The checked name of a series, and the `metric!` macro that builds a
//! metric under one.
//!
//! A [`Name`] is tied to the unit of the value it is pushed with, and its
//! one constructor refuses a text that breaks a naming rule. Every name is
//! built in a `const` context, so a bad name stops the build and no review
//! has to catch it.

use core::marker::PhantomData;

use ubos_core::units::{
    Current, Difficulty, FanPwm, FanRpm, Frequency, Power, Shares, Temperature, Voltage,
};

use crate::value::{Count, Flag, Ratio, Value};

#[cfg(test)]
#[path = "tests/name.rs"]
mod tests;

/// The suffix of a temperature series.
const CELSIUS: &str = "_celsius";
/// The suffix of a voltage series.
const VOLTS: &str = "_volts";
/// The suffix of a power series.
const WATTS: &str = "_watts";
/// The suffix of a current series.
const AMPERES: &str = "_amperes";
/// The suffix of a frequency series.
const HERTZ: &str = "_hertz";
/// The suffix of a ratio series.
const RATIO: &str = "_ratio";
/// The suffix of a series of mining work.
const SHARES: &str = "_shares";
/// The suffix of a series of a target difficulty.
const DIFFICULTY: &str = "_difficulty";

/// Every suffix that states the unit of a typed value. A name whose unit
/// has no suffix must end in none of them.
const UNIT_SUFFIXES: [&str; 8] = [
    CELSIUS, VOLTS, WATTS, AMPERES, HERTZ, RATIO, SHARES, DIFFICULTY,
];

/// The endings that state a scaled unit or a percentage. A series is in
/// its base unit, so no name ends in one.
const SCALED_SUFFIXES: [&str; 8] = [
    "_mv", "_mw", "_ma", "_khz", "_mhz", "_ms", "_us", "_percent",
];

/// The prefixes of a last word that state a scale, such as
/// `millicelsius`.
const SCALE_WORDS: [&str; 3] = ["milli", "micro", "kilo"];

/// The prefix that every series once carried. It says nothing, because
/// every series comes from a board.
const BOARD_PREFIX: &str = "board_";

/// The prefix that is allowed only on a name in [`HASHBOARD_NOUN`].
const HASHBOARD_PREFIX: &str = "hashboard_";

/// The names whose noun is the hashboard itself. Every other series of a
/// hashboard says so with the `hashboard` label and carries no prefix.
const HASHBOARD_NOUN: [&str; 2] = ["hashboard_present", "hashboard_shares"];

/// A value type that a series can hold, with the suffix its name ends in.
///
/// The suffix is on the type so that [`Name::new`] can compare a name
/// with the unit of the value it is pushed with.
pub trait Unit: Into<Value> {
    /// The ending of every name of this unit, or the empty text for a
    /// unit that a name does not state.
    const SUFFIX: &'static str;
}

impl Unit for Temperature {
    const SUFFIX: &'static str = CELSIUS;
}

impl Unit for Voltage {
    const SUFFIX: &'static str = VOLTS;
}

impl Unit for Power {
    const SUFFIX: &'static str = WATTS;
}

impl Unit for Current {
    const SUFFIX: &'static str = AMPERES;
}

impl Unit for Frequency {
    const SUFFIX: &'static str = HERTZ;
}

impl Unit for Ratio {
    const SUFFIX: &'static str = RATIO;
}

impl Unit for Shares {
    const SUFFIX: &'static str = SHARES;
}

impl Unit for Difficulty {
    const SUFFIX: &'static str = DIFFICULTY;
}

impl Unit for FanPwm {
    const SUFFIX: &'static str = "";
}

impl Unit for FanRpm {
    const SUFFIX: &'static str = "";
}

/// A count has no unit, so its name states none.
impl Unit for Count {
    const SUFFIX: &'static str = "";
}

/// A flag has no unit, so its name states none.
impl Unit for Flag {
    const SUFFIX: &'static str = "";
}

/// The name of a series whose value has the unit `U`.
///
/// The unit is part of the type, so a metric cannot pair a name with a
/// value of another unit. The marker is a function pointer, which keeps
/// the name `Copy`, `Send` and `Sync` for every unit.
pub struct Name<U> {
    text: &'static str,
    unit: PhantomData<fn() -> U>,
}

impl<U> Clone for Name<U> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<U> Copy for Name<U> {}

impl<U> Name<U> {
    /// The text of the name, as a scrape shows it.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.text
    }
}

impl<U: Unit> Name<U> {
    /// Check `text` against the naming rules and the unit `U`, and wrap
    /// it.
    ///
    /// Call it in a `const` context, which the `metric!` macro and a
    /// `const` item both are, so that a refused name stops the build:
    ///
    /// ```
    /// use ubos_core::units::Voltage;
    /// use ubos_metrics::Name;
    ///
    /// const SET_VOLTAGE: Name<Voltage> = Name::new("psu_set_voltage_volts");
    /// assert_eq!(SET_VOLTAGE.as_str(), "psu_set_voltage_volts");
    /// ```
    ///
    /// A name that states another unit than its value has does not build:
    ///
    /// ```compile_fail
    /// use ubos_core::units::Voltage;
    /// use ubos_metrics::Name;
    ///
    /// const SET_VOLTAGE: Name<Voltage> = Name::new("psu_set_voltage_celsius");
    /// ```
    ///
    /// # Panics
    ///
    /// Panics when `text` breaks a rule. In a `const` context the panic
    /// is a compile error at the line that names the series.
    #[must_use]
    pub const fn new(text: &'static str) -> Self {
        let bytes = text.as_bytes();
        check_form(bytes);
        check_prefix(bytes);
        check_base_unit(bytes);
        check_unit(bytes, U::SUFFIX.as_bytes());
        Self {
            text,
            unit: PhantomData,
        }
    }
}

/// Refuse a text that is not lowercase words joined by single
/// underscores, the form of every series name.
const fn check_form(text: &[u8]) {
    assert!(!text.is_empty(), "the name is empty");
    assert!(
        text[0].is_ascii_lowercase(),
        "the name does not start with a lowercase letter"
    );
    let mut index = 0;
    while index < text.len() {
        let byte = text[index];
        assert!(
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_',
            "the name has a character that is not a lowercase letter, a digit or an underscore"
        );
        assert!(
            byte != b'_' || text[index - 1] != b'_',
            "the name has two underscores in a row"
        );
        index += 1;
    }
    assert!(
        text[text.len() - 1] != b'_',
        "the name ends in an underscore"
    );
}

/// Refuse the `board_` prefix, and the `hashboard_` prefix on a name
/// that is not in [`HASHBOARD_NOUN`].
const fn check_prefix(text: &[u8]) {
    assert!(
        !starts_with(text, BOARD_PREFIX.as_bytes()),
        "the name starts with the prefix board_"
    );
    if !starts_with(text, HASHBOARD_PREFIX.as_bytes()) {
        return;
    }
    let mut index = 0;
    while index < HASHBOARD_NOUN.len() {
        if equals(text, HASHBOARD_NOUN[index].as_bytes()) {
            return;
        }
        index += 1;
    }
    panic!("the name starts with the prefix hashboard_ and is not in the list HASHBOARD_NOUN");
}

/// Refuse a name that ends in a scaled unit or a percentage.
const fn check_base_unit(text: &[u8]) {
    let mut index = 0;
    while index < SCALED_SUFFIXES.len() {
        assert!(
            !ends_with(text, SCALED_SUFFIXES[index].as_bytes()),
            "the name ends in a unit that is not a base unit"
        );
        index += 1;
    }
    let word = last_word_start(text);
    let mut index = 0;
    while index < SCALE_WORDS.len() {
        assert!(
            !starts_with_at(text, word, SCALE_WORDS[index].as_bytes()),
            "the name ends in a unit that is not a base unit"
        );
        index += 1;
    }
}

/// Refuse a name whose ending and the unit suffix `suffix` disagree. A
/// unit with a suffix needs it, and a unit with none takes no ending
/// that belongs to another unit.
const fn check_unit(text: &[u8], suffix: &[u8]) {
    if !suffix.is_empty() {
        assert!(
            ends_with(text, suffix),
            "the name does not end in the unit of the value"
        );
        return;
    }
    let mut index = 0;
    while index < UNIT_SUFFIXES.len() {
        assert!(
            !ends_with(text, UNIT_SUFFIXES[index].as_bytes()),
            "the name ends in a unit that does not fit the value"
        );
        index += 1;
    }
}

/// The index of the first byte of the last word of `text`, the one after
/// its last underscore.
const fn last_word_start(text: &[u8]) -> usize {
    let mut index = text.len();
    while index > 0 {
        if text[index - 1] == b'_' {
            return index;
        }
        index -= 1;
    }
    0
}

/// Whether `text` holds `prefix` at the byte `offset`.
const fn starts_with_at(text: &[u8], offset: usize, prefix: &[u8]) -> bool {
    if text.len() < offset + prefix.len() {
        return false;
    }
    let mut index = 0;
    while index < prefix.len() {
        if text[offset + index] != prefix[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Whether `text` begins with `prefix`.
const fn starts_with(text: &[u8], prefix: &[u8]) -> bool {
    starts_with_at(text, 0, prefix)
}

/// Whether `text` ends with `suffix`.
const fn ends_with(text: &[u8], suffix: &[u8]) -> bool {
    text.len() >= suffix.len() && starts_with_at(text, text.len() - suffix.len(), suffix)
}

/// Whether `text` and `other` hold the same bytes.
const fn equals(text: &[u8], other: &[u8]) -> bool {
    text.len() == other.len() && starts_with(text, other)
}

/// Build a [`Metric`](crate::Metric) from a name and a value of its unit.
///
/// A literal name is checked here, in a `const` block that takes its unit
/// from the value, so `metric!("fan_rpm", rpm)` needs no declared name. A
/// declared [`Name`] was checked where it was declared and passes through,
/// which is why the macro has a second arm.
#[macro_export]
macro_rules! metric {
    ($name:literal, $value:expr) => {
        $crate::Metric::new(const { $crate::Name::new($name) }, $value)
    };
    ($name:expr, $value:expr) => {
        $crate::Metric::new($name, $value)
    };
}
