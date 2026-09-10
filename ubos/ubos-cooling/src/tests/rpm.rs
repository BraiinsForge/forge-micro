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

//! `pulses_to_rpm` at its edges: a normal window, an empty window, a zero
//! window, a zero divisor and a saturating count.

use crate::pulses_to_rpm;

const TWO_PULSES_PER_REV: u64 = 2;
const ONE_SECOND_US: u64 = 1_000_000;

#[test]
fn one_second_window() {
    // 100 pulses at two per revolution over one second is 50 revolutions
    // per second, so 3000 RPM.
    assert_eq!(
        pulses_to_rpm(100, TWO_PULSES_PER_REV, ONE_SECOND_US).rpm(),
        3000
    );
}

#[test]
fn no_pulses_is_zero() {
    assert_eq!(pulses_to_rpm(0, TWO_PULSES_PER_REV, ONE_SECOND_US).rpm(), 0);
}

#[test]
fn zero_window_is_zero() {
    assert_eq!(pulses_to_rpm(100, TWO_PULSES_PER_REV, 0).rpm(), 0);
}

#[test]
fn saturates_at_u32_max() {
    assert_eq!(pulses_to_rpm(u64::from(u32::MAX), 1, 1).rpm(), u32::MAX);
}

#[test]
fn zero_pulses_per_rev_is_zero() {
    assert_eq!(pulses_to_rpm(100, 0, ONE_SECOND_US).rpm(), 0);
}
