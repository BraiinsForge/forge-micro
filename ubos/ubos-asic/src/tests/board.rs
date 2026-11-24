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
fn bmm101_max_rail_voltage_clamps_to_psu_ceiling() {
    // Chip stack 4 * 422 mV = 1688 mV exceeds the BMM101_HASHBOARD PSU's
    // configured upper bound 1675 mV; max_rail_voltage clamps to the
    // PSU ceiling so Psu::set_voltage never returns Error::OutOfRange
    // when the tuner anchors the open-core rail at this value.
    assert_eq!(BMM101_HASHBOARD.max_rail_voltage(), Voltage::from_mv(1675));
}

/// The tuners and the input power of the boards that carry the BMM101.
const TUNERS: &[TunerInfo] = &[
    TunerInfo::Fixed { name: "Fixed" },
    TunerInfo::Power {
        name: "PowerRamp",
        reads_efficiency: true,
    },
];

/// The bounds of a BMM101 machine with both tuners.
static BMM101_BOUNDS: MiningBounds = BMM101_HASHBOARD.bounds(
    TUNERS,
    Some(Power::from_mw(19_000)..=Power::from_mw(67_000)),
);

/// The values a BMM101 hands to a new config are the ones the board has
/// always started on.
#[test]
fn bmm101_defaults_hold_its_start_values() {
    assert_eq!(
        BMM101_HASHBOARD.defaults,
        HashboardDefaults {
            frequency: Frequency::from_mhz(525),
            voltage: Voltage::from_mv(1400),
            efficiency: Efficiency::from_mj_per_th(NonZeroU32::new(40_000).expect("nonzero")),
            dangerous: TemperatureDescriptor {
                chip: None,
                hashboard: Some(Temperature::from_celsius(83)),
            },
            cooling_target: PidTarget {
                source: Source::Hashboard,
                statistic: Statistic::Max,
                temp: Temperature::from_celsius(60),
            },
            min_fan_pwm: FanPwm::from_percent(20),
            input_smoother_half_decay_secs: 0,
        }
    );
}

/// The values the hashnode hashboard hands to a new config are the ones
/// the board has always started on.
#[test]
fn bfmx4_defaults_hold_its_start_values() {
    assert_eq!(
        BFMX4_HASHBOARD.defaults,
        HashboardDefaults {
            frequency: Frequency::from_mhz(600),
            voltage: Voltage::from_mv(4550),
            efficiency: Efficiency::from_mj_per_th(NonZeroU32::new(15_000).expect("nonzero")),
            dangerous: TemperatureDescriptor {
                chip: Some(Temperature::from_celsius(110)),
                hashboard: Some(Temperature::from_celsius(75)),
            },
            cooling_target: PidTarget {
                source: Source::Hashboard,
                statistic: Statistic::Max,
                temp: Temperature::from_celsius(47),
            },
            min_fan_pwm: FanPwm::from_percent(20),
            input_smoother_half_decay_secs: 0,
        }
    );
}

/// The bounds take the frequency range of the chip and the rail ceiling
/// of the board, with a floor of zero.
#[test]
fn bounds_read_the_chip_range_and_the_rail_ceiling() {
    assert_eq!(
        BMM101_BOUNDS.frequency,
        Frequency::from_mhz(50)..=Frequency::from_mhz(1000)
    );
    assert_eq!(
        BMM101_BOUNDS.voltage,
        Voltage::from_mv(0)..=Voltage::from_mv(1675)
    );
    assert_eq!(
        BMM101_BOUNDS.input_power,
        Some(Power::from_mw(19_000)..=Power::from_mw(67_000))
    );
    assert_eq!(BMM101_BOUNDS.tuners, TUNERS);
}

/// The mining metadata names the chip and the board, and carries the
/// defaults of the board.
#[test]
fn mining_info_reads_the_identity_and_the_defaults() {
    let info = BMM101_HASHBOARD.mining_info(&BMM101_BOUNDS, DefaultTuning::Fixed);
    assert_eq!(info.asic_model, "BM1362");
    assert_eq!(info.device_model, "miniminer");
    assert_eq!(info.swarm_color, "blue");
    assert_eq!(info.asic_count, 4);
    assert_eq!(info.default_frequency, Frequency::from_mhz(525));
    assert_eq!(info.default_voltage, Voltage::from_mv(1400));
    assert_eq!(
        info.default_efficiency,
        Efficiency::from_mj_per_th(NonZeroU32::new(40_000).expect("nonzero"))
    );
    assert_eq!(info.default_tuning, DefaultTuning::Fixed);
}
