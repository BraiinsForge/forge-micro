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
    // 0 °C: high = 0x00, low = 0x00 → 0 mC
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
    // 50 °C: high = 0x32 (50), low = 0x00 (0/16 frac) → 50_000 mC
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
    // 50.5 °C: high = 0x32 (50), low = 0x80 (8/16 = 0.5 °C) → 50_000 + 500 = 50_500 mC
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
    // 50.0625 °C: high = 0x32 (50), low = 0x10 (1/16) → 50_000 + 62 mC
    // 625 / 10 = 62 (integer truncation)
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
    // 127.9375 °C: high = 0x7F (127), low = 0xF0 (15/16)
    // 127_000 + 15 * 625 / 10 = 127_000 + 937 = 127_937 mC
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
    // 85 °C: high = 0x55, low = 0x00 → 85_000 mC
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
    // high = 0xC0 in standard range is −64 °C (2's complement), the
    // open/unbiased remote-diode reading — not +192 °C. Out of range, so
    // the caller rejects it and falls back to the hashboard readings.
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
    // Same 0xC0 in extended range is 192 − 64 = 128 °C.
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0xC0,
            low: 0x00,
            extended: true,
        }),
        Temperature::from_millicelsius(128_000)
    );
    // 0x40 in extended range is the zero point (64 − 64 = 0 °C).
    assert_eq!(
        Temperature::from(RawTmp461 {
            high: 0x40,
            low: 0x00,
            extended: true,
        }),
        Temperature::from_millicelsius(0)
    );
}
