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
    // 0 °C: buf = [0x00, 0x00] → raw12 = 0 → 0 mC
    assert_eq!(
        Temperature::from(RawTmp102([0x00, 0x00])),
        Temperature::from_millicelsius(0)
    );
}

#[test]
fn positive_integer_degree() {
    // 25 °C: 25 / 0.0625 = 400 = 0x190, left-justified → 0x1900
    // buf = [0x19, 0x00] → raw12 = 0x190 = 400 → 400 * 125 / 2 = 25_000 mC
    assert_eq!(
        Temperature::from(RawTmp102([0x19, 0x00])),
        Temperature::from_millicelsius(25_000)
    );
}

#[test]
fn positive_fractional_degree() {
    // 25.0625 °C: 25 + 1 LSB = 401 = 0x191, left-justified → 0x1910
    // buf = [0x19, 0x10] → raw12 = 0x191 = 401 → 401 * 125 / 2 = 25_062 mC
    // (truncated integer division, not rounded)
    assert_eq!(
        Temperature::from(RawTmp102([0x19, 0x10])),
        Temperature::from_millicelsius(25_062)
    );
}

#[test]
fn max_positive() {
    // 127.9375 °C: 2047 = 0x7FF, left-justified → 0x7FF0
    // raw12 = 0x7FF = 2047 → 2047 * 125 / 2 = 127_937 mC
    assert_eq!(
        Temperature::from(RawTmp102([0x7F, 0xF0])),
        Temperature::from_millicelsius(127_937)
    );
}

#[test]
fn negative_integer_degree() {
    // −25 °C: raw12 = −25 / 0.0625 = −400.
    // Left-justified 16-bit word = raw12 << 4 = −400 * 16 = −6400 as i16 = 0xE700.
    // buf = [0xE7, 0x00] → i16::from_be_bytes = −6400; >> 4 = −400
    // −400 * 125 / 2 = −25_000 mC
    assert_eq!(
        Temperature::from(RawTmp102([0xE7, 0x00])),
        Temperature::from_millicelsius(-25_000)
    );
}

#[test]
fn negative_half_degree() {
    // −0.5 °C: −8 raw12; 12-bit two's complement = 0xFF8, left-justified = 0xFF80
    // buf = [0xFF, 0x80] → raw16 = 0xFF80_i16 = −128; >> 4 = −8
    // −8 * 125 / 2 = −500 mC
    assert_eq!(
        Temperature::from(RawTmp102([0xFF, 0x80])),
        Temperature::from_millicelsius(-500)
    );
}
