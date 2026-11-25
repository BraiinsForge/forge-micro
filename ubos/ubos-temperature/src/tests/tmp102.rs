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
        Temperature::from(RawTmp102([0x00, 0x00])),
        Temperature::from_millicelsius(0)
    );
}

#[test]
fn positive_integer_degree() {
    assert_eq!(
        Temperature::from(RawTmp102([0x19, 0x00])),
        Temperature::from_millicelsius(25_000)
    );
}

#[test]
fn positive_fractional_degree() {
    // The division truncates rather than rounds.
    assert_eq!(
        Temperature::from(RawTmp102([0x19, 0x10])),
        Temperature::from_millicelsius(25_062)
    );
}

#[test]
fn max_positive() {
    assert_eq!(
        Temperature::from(RawTmp102([0x7F, 0xF0])),
        Temperature::from_millicelsius(127_937)
    );
}

#[test]
fn negative_integer_degree() {
    assert_eq!(
        Temperature::from(RawTmp102([0xE7, 0x00])),
        Temperature::from_millicelsius(-25_000)
    );
}

#[test]
fn negative_half_degree() {
    assert_eq!(
        Temperature::from(RawTmp102([0xFF, 0x80])),
        Temperature::from_millicelsius(-500)
    );
}
