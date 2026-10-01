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
fn frequency_from_mhz_saturates_above_u32_max_khz() {
    // Inputs above u32::MAX / 1000 would overflow the kHz scalar.
    // saturating_mul caps at u32::MAX rather than wrapping, so a
    // corrupted input cannot present as a plausibly low clock.
    let max_mhz = u32::MAX / 1000;
    assert_eq!(Frequency::from_mhz(max_mhz).khz(), max_mhz * 1000);
    assert_eq!(Frequency::from_mhz(max_mhz + 1).khz(), u32::MAX);
    assert_eq!(Frequency::from_mhz(u32::MAX).khz(), u32::MAX);
}

/// Four chips of 514 cores draw 40 W at 40 J/TH at 486.38 MHz. A zero
/// core count, or a value past its scalar, gives none.
#[test]
fn frequency_try_from_power_and_efficiency_spreads_the_hashrate_over_the_cores() {
    for (mw, mj_per_th, cores, expected) in [
        (40_000, 40_000, 4 * 514, Some(486_381)),
        (40_000, 40_000, 0, None),
        (u32::MAX, 1, 1, None),
    ] {
        let frequency = Frequency::try_from_power_and_efficiency(
            Power::from_mw(mw),
            Efficiency::from_mj_per_th(core::num::NonZeroU32::new(mj_per_th).expect("nonzero")),
            cores,
        );
        assert_eq!(frequency.map(Frequency::khz), expected);
    }
}

#[test]
fn frequency_range_reports_its_inclusive_bounds() {
    // The descriptors type their operating envelope with this range and
    // the register calculators read both bounds back to reject targets
    // outside it, so min/max must return the values new() was handed.
    let range = FrequencyRange::new(Frequency::from_mhz(50), Frequency::from_mhz(1000));
    assert_eq!(range.min(), Frequency::from_mhz(50));
    assert_eq!(range.max(), Frequency::from_mhz(1000));
}

/// Clock-grid rounding must preserve aligned values and floor fractional MHz.
#[test]
fn frequency_snap_down_floors_to_step_grid() {
    let step = Frequency::from_khz(6_250);
    for (input, expected) in [(0, 0), (6_249, 0), (550_000, 550_000), (551_000, 550_000)] {
        assert_eq!(Frequency::from_khz(input).snap_down(step).khz(), expected);
    }
}

/// Ramps must converge from either direction without crossing the target or wrapping.
#[test]
fn frequency_step_toward_moves_without_overshoot() {
    for (current, target, step, expected) in [
        (100, 200, 25, 125),
        (200, 100, 25, 175),
        (100, 110, 25, 110),
        (110, 100, 25, 100),
        (100, 100, 25, 100),
        (10, 0, 25, 0),
        (u32::MAX - 10, u32::MAX, 25, u32::MAX),
    ] {
        assert_eq!(
            Frequency::from_khz(current)
                .step_toward(Frequency::from_khz(target), Frequency::from_khz(step))
                .khz(),
            expected,
        );
    }
}

#[test]
#[should_panic(expected = "BUG: frequency step must be nonzero")]
fn frequency_snap_down_rejects_zero() {
    let _ = Frequency::from_khz(100).snap_down(Frequency::from_khz(0));
}

#[test]
#[should_panic(expected = "BUG: frequency step must be nonzero")]
fn frequency_step_toward_rejects_zero() {
    let _ = Frequency::from_khz(100).step_toward(Frequency::from_khz(200), Frequency::from_khz(0));
}
