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

// Four chips of 514 cores draw 40 W at 40 J/TH at 486.38 MHz. A zero
// core count, or a value past its scalar, gives none.
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
    let range = FrequencyRange::new(Frequency::from_mhz(50), Frequency::from_mhz(1000));
    assert_eq!(range.min(), Frequency::from_mhz(50));
    assert_eq!(range.max(), Frequency::from_mhz(1000));
}

#[test]
fn frequency_snap_down_floors_to_step_grid() {
    let step = Frequency::from_khz(6_250);
    for (input, expected) in [(0, 0), (6_249, 0), (550_000, 550_000), (551_000, 550_000)] {
        assert_eq!(Frequency::from_khz(input).snap_down(step).khz(), expected);
    }
}

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

#[test]
fn a_temperature_displays_in_degrees() {
    extern crate std;
    use std::string::ToString;
    let shown = |millicelsius| Temperature::from_millicelsius(millicelsius).to_string();
    assert_eq!(shown(95_125), "95.125");
    assert_eq!(shown(5), "0.005");
    assert_eq!(shown(-1_500), "-1.500");
    assert_eq!(shown(-500), "-0.500");
}

#[test]
fn a_fractional_temperature_rounds_to_the_nearest_millicelsius() {
    let millicelsius = |celsius| Temperature::from_celsius_f64(celsius).millicelsius();
    assert_eq!(millicelsius(41.5), 41_500);
    assert_eq!(millicelsius(41.4996), 41_500);
    assert_eq!(millicelsius(41.4994), 41_499);
    assert_eq!(millicelsius(-1.4996), -1_500);
}

#[test]
fn current_is_built_from_amps_and_milliamps() {
    assert_eq!(Current::from_amps(3).ma(), 3000);
    assert_eq!(Current::from_ma(1500).ma(), 1500);
    assert_eq!(Current::from_amps(u32::MAX).ma(), u32::MAX);
}

#[test]
fn power_converts_to_watts() {
    assert!((Power::from_mw(2500).as_watts() - 2.5).abs() < 1e-9);
}

#[test]
fn a_fractional_power_rounds_to_the_nearest_milliwatt() {
    assert_eq!(Power::from_watts_f64(2.5004), Power::from_mw(2_500));
    assert_eq!(Power::from_watts_f64(2.5006), Power::from_mw(2_501));
    assert_eq!(Power::from_watts_f64(-3.0), Power::from_mw(0));
    assert_eq!(Power::from_watts_f64(f64::NAN), Power::from_mw(0));
    assert_eq!(Power::from_watts_f64(1.0e12), Power::from_mw(u32::MAX));
}

#[test]
fn a_fractional_frequency_rounds_to_the_nearest_kilohertz() {
    assert_eq!(
        Frequency::from_mhz_f64(100.0004),
        Frequency::from_khz(100_000)
    );
    assert_eq!(
        Frequency::from_mhz_f64(100.0006),
        Frequency::from_khz(100_001)
    );
    assert_eq!(Frequency::from_mhz_f64(-5.0), Frequency::from_khz(0));
    assert_eq!(Frequency::from_mhz_f64(f64::NAN), Frequency::from_khz(0));
    assert_eq!(
        Frequency::from_mhz_f64(1.0e12),
        Frequency::from_khz(u32::MAX)
    );
}

// A unit with no reading is zero, which the API reports where a source
// has not published yet.
#[test]
fn default_units_are_zero() {
    assert_eq!(Frequency::default().khz(), 0);
    assert_eq!(Voltage::default().mv(), 0);
    assert_eq!(Power::default().mw(), 0);
    assert_eq!(Current::default().ma(), 0);
    assert_eq!(FanPwm::default(), FanPwm::ZERO);
    assert_eq!(FanRpm::default().rpm(), 0);
}

// A unit shown in thousandths of a larger unit writes three decimals of
// it, so a 6.25 MHz step and a millivolt both survive the log line.
#[test]
fn a_scaled_unit_displays_with_three_decimals() {
    extern crate std;
    use std::string::ToString;
    assert_eq!(Frequency::from_khz(487_500).to_string(), "487.500");
    assert_eq!(Frequency::from_khz(0).to_string(), "0.000");
    assert_eq!(Voltage::from_mv(1_405).to_string(), "1.405");
    assert_eq!(Power::from_mw(44_000).to_string(), "44.000");
    assert_eq!(Power::from_mw(u32::MAX).to_string(), "4294967.295");
    assert_eq!(Current::from_ma(25_500).to_string(), "25.500");
    assert_eq!(Current::from_ma(7).to_string(), "0.007");
    let efficiency =
        Efficiency::from_mj_per_th(core::num::NonZeroU32::new(40_001).expect("nonzero"));
    assert_eq!(efficiency.to_string(), "40.001");
}
