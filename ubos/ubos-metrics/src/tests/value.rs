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

//! Coverage for the text of a [`Value`]: each variant writes its base
//! unit, and no text is wider than the line budget allows.

extern crate std;

use std::string::{String, ToString};

use ubos_core::units::{Current, FanPwm, FanRpm, Frequency, Power, Temperature, Voltage};

use super::{Ratio, Value};
use crate::store::MAX_INTEGER_CHARS;

/// The text of a value built from the unit given.
fn shown(value: impl Into<Value>) -> String {
    value.into().to_string()
}

/// Each variant writes its base unit, and a unit held in thousandths
/// keeps its trailing zeros.
#[test]
fn each_variant_displays_its_base_unit() {
    assert_eq!(shown(Voltage::from_mv(21_600)), "21.600");
    assert_eq!(shown(Temperature::from_millicelsius(-25_500)), "-25.500");
    assert_eq!(shown(Power::from_mw(44_000)), "44.000");
    assert_eq!(shown(Current::from_ma(25_500)), "25.500");
    assert_eq!(shown(Frequency::from_khz(1_500)), "1500000");
    assert_eq!(shown(FanPwm::from_percent(40)), "40");
    assert_eq!(shown(FanRpm::from_rpm(4_200)), "4200");
    assert_eq!(shown(Ratio::new(1, 4)), "0.250");
    assert_eq!(shown(Value::Flag(true)), "1");
    assert_eq!(shown(Value::Flag(false)), "0");
    assert_eq!(shown(Value::Count(1_234)), "1234");
}

/// A negative reading below one degree keeps its sign, which the whole
/// part alone would lose.
#[test]
fn a_negative_value_under_one_unit_keeps_its_sign() {
    assert_eq!(shown(Temperature::from_millicelsius(-500)), "-0.500");
}

/// The line budget counts `MAX_INTEGER_CHARS` for the value, so the
/// widest text of every variant has to fit it.
#[test]
fn the_widest_text_of_each_variant_fits_the_line_budget() {
    let widest = [
        shown(Voltage::from_mv(u32::MAX)),
        shown(Temperature::from_millicelsius(i32::MIN)),
        shown(Temperature::from_millicelsius(i32::MAX)),
        shown(Power::from_mw(u32::MAX)),
        shown(Current::from_ma(u32::MAX)),
        shown(Frequency::from_khz(u32::MAX)),
        shown(FanPwm::FULL),
        shown(FanRpm::from_rpm(u32::MAX)),
        shown(Ratio::new(1, 1)),
        shown(Value::Count(u64::MAX)),
    ];
    for text in widest {
        assert!(
            text.len() <= MAX_INTEGER_CHARS,
            "{text} is wider than the line budget"
        );
    }
    assert_eq!(
        shown(Temperature::from_millicelsius(i32::MIN)),
        "-2147483.648"
    );
    assert_eq!(shown(Voltage::from_mv(u32::MAX)), "4294967.295");
    assert_eq!(shown(Value::Count(u64::MAX)), "18446744073709551615");
}

/// The integer a text states once its decimal point is dropped, which
/// for a unit held in thousandths is the number the unit holds.
fn digits(text: &str) -> i128 {
    text.replace('.', "")
        .parse()
        .expect("a value renders as a decimal number")
}

/// Values at the edges of each range and at every digit boundary between
/// them.
const SWEEP: [u32; 12] = [
    0,
    1,
    9,
    10,
    99,
    999,
    1_000,
    1_001,
    21_600,
    999_999,
    u32::MAX - 1,
    u32::MAX,
];

/// The text of every variant parses back to the integer it was built
/// from, against a reference wide enough that it cannot overflow.
#[test]
fn a_sweep_of_values_parses_back_to_the_integer_it_holds() {
    for raw in SWEEP {
        let reference = i128::from(raw);
        assert_eq!(digits(&shown(Voltage::from_mv(raw))), reference);
        assert_eq!(digits(&shown(Power::from_mw(raw))), reference);
        assert_eq!(digits(&shown(Current::from_ma(raw))), reference);
        assert_eq!(digits(&shown(FanRpm::from_rpm(raw))), reference);
        assert_eq!(
            digits(&shown(Frequency::from_khz(raw))),
            reference * i128::from(super::HERTZ_PER_KILOHERTZ)
        );
        assert_eq!(digits(&shown(Value::Count(u64::from(raw)))), reference);

        let signed = raw.cast_signed();
        for millicelsius in [signed, signed.wrapping_neg()] {
            assert_eq!(
                digits(&shown(Temperature::from_millicelsius(millicelsius))),
                i128::from(millicelsius)
            );
        }
    }
}

/// A ratio rounds to the nearest thousandth and stays between 0 and 1
/// for a window with no elapsed time or a part that ran ahead.
#[test]
fn a_ratio_rounds_to_the_nearest_thousandth_and_stays_in_range() {
    assert_eq!(shown(Ratio::new(5, 0)), "0.000");
    assert_eq!(shown(Ratio::new(7, 5)), "1.000");
    assert_eq!(shown(Ratio::new(1, 2)), "0.500");
    assert_eq!(shown(Ratio::new(2, 3)), "0.667");
    assert_eq!(shown(Ratio::new(1, 3)), "0.333");
    assert_eq!(shown(Ratio::new(u64::MAX, u64::MAX)), "1.000");
    assert_eq!(shown(Ratio::new(u64::MAX / 2, u64::MAX)), "0.500");
}

/// A value is a tag and eight bytes, so a stored series and a queued
/// metric keep the size they had.
#[test]
fn a_value_takes_sixteen_bytes() {
    assert_eq!(size_of::<Value>(), 16);
}
