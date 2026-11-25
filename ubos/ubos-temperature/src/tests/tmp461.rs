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

use super::*;

#[test]
fn zero() {
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x00,
            low: 0x00,
            extended: false,
        }),
        Temperature::from_millicelsius(0)
    );
}

#[test]
fn integer_degree() {
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x32,
            low: 0x00,
            extended: false,
        }),
        Temperature::from_millicelsius(50_000)
    );
}

#[test]
fn half_degree() {
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x32,
            low: 0x80,
            extended: false,
        }),
        Temperature::from_millicelsius(50_500)
    );
}

#[test]
fn one_sixteenth_degree() {
    // The fractional step truncates rather than rounds.
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x32,
            low: 0x10,
            extended: false,
        }),
        Temperature::from_millicelsius(50_062)
    );
}

#[test]
fn max_standard() {
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x7F,
            low: 0xF0,
            extended: false,
        }),
        Temperature::from_millicelsius(127_937)
    );
}

#[test]
fn typical_die_temp() {
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x55,
            low: 0x00,
            extended: false,
        }),
        Temperature::from_millicelsius(85_000)
    );
}

#[test]
fn standard_range_is_twos_complement() {
    // In standard range 0xC0 is −64 °C in two's complement, not +192 °C:
    // the open remote-diode reading, which is out of range.
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0xC0,
            low: 0x00,
            extended: false,
        }),
        Temperature::from_millicelsius(-64_000)
    );
}

#[test]
fn extended_range_applies_offset() {
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0xC0,
            low: 0x00,
            extended: true,
        }),
        Temperature::from_millicelsius(128_000)
    );
    // 0x40 is the extended-range zero point.
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x40,
            low: 0x00,
            extended: true,
        }),
        Temperature::from_millicelsius(0)
    );
}
