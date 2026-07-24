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

//! Unit tests for [`super::stale_timeout`].

use super::{Duration, MIN_STALE_TIMEOUT, STALE_INTERVAL_MULTIPLIER, stale_timeout};

/// `thermal.sample_interval_ms` is validated over `100..=60_000`; the
/// fastest configured rate scales to well below [`MIN_STALE_TIMEOUT`],
/// so the floor applies.
#[test]
fn floor_applies_at_min_sample_interval() {
    assert_eq!(stale_timeout(100), MIN_STALE_TIMEOUT);
}

/// A sample interval large enough that the scaled value clears the floor
/// yields exactly `sample_interval * STALE_INTERVAL_MULTIPLIER`, both at
/// an ordinary interval and at the slowest interval
/// `thermal.sample_interval_ms` validation permits.
#[test]
fn scales_by_multiplier_above_the_floor() {
    // Pin the multiplier and the resulting timeouts to concrete literals.
    // Reconstructing the expected value from the same `STALE_INTERVAL_MULTIPLIER`
    // the SUT reads would pass even if the constant were set wrong, so assert
    // the constant itself and the ×7 outputs directly.
    assert_eq!(STALE_INTERVAL_MULTIPLIER, 7);
    assert_eq!(stale_timeout(1_000), Duration::from_millis(7_000));
    assert_eq!(stale_timeout(60_000), Duration::from_millis(420_000));
    assert!(stale_timeout(60_000) > MIN_STALE_TIMEOUT);
}
