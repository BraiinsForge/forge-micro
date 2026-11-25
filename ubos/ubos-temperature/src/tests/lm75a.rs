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

/// Encode eighths of a degree as the two-byte register value.
fn encode(eighths_of_degree: i16) -> [u8; 2] {
    (eighths_of_degree << 5).to_be_bytes()
}

#[test]
fn zero() {
    assert_eq!(
        Temperature::from(RawLm75a(encode(0))),
        Temperature::from_millicelsius(0)
    );
}

#[test]
fn positive_integer_degree() {
    assert_eq!(
        Temperature::from(RawLm75a(encode(25 * 8))),
        Temperature::from_millicelsius(25_000)
    );
}

#[test]
fn positive_half_degree() {
    assert_eq!(
        Temperature::from(RawLm75a(encode(25 * 8 + 4))),
        Temperature::from_millicelsius(25_500)
    );
}

#[test]
fn positive_eighth_degree() {
    assert_eq!(
        Temperature::from(RawLm75a(encode(25 * 8 + 1))),
        Temperature::from_millicelsius(25_125)
    );
}

#[test]
fn negative_integer_degree() {
    assert_eq!(
        Temperature::from(RawLm75a(encode(-25 * 8))),
        Temperature::from_millicelsius(-25_000)
    );
}

#[test]
fn negative_half_degree() {
    assert_eq!(
        Temperature::from(RawLm75a(encode(-25 * 8 - 4))),
        Temperature::from_millicelsius(-25_500)
    );
}
