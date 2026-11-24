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

//! The seed, the bounds and the config that the tests of this crate share.
//!
//! The values are written out here, so each test states the config it
//! runs on and the crate reads no hashboard descriptor to get one. They
//! are the values that the golden records in the storage tests encode.

use core::num::NonZeroU32;
use core::ops::RangeInclusive;

use ubos_core::units::{Efficiency, FanPwm, Frequency, Power, Temperature, Voltage};

use crate::{
    Config, DefaultTuning, HashboardDefaults, MiningBounds, MiningInfo, PidTarget, Source,
    Statistic, TemperatureDescriptor, TunerInfo,
};

/// The seed of a hashboard that reads a hashboard sensor and no die.
pub(crate) const DEFAULTS: HashboardDefaults = HashboardDefaults {
    frequency: Frequency::from_mhz(525),
    voltage: Voltage::from_mv(1400),
    efficiency: Efficiency::from_mj_per_th(
        NonZeroU32::new(40_000).expect("BUG: efficiency is zero"),
    ),
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
};

/// The frequency range of the chip that the bounds describe.
const FREQUENCY: RangeInclusive<Frequency> = Frequency::from_mhz(50)..=Frequency::from_mhz(1000);

/// The highest rail voltage that the bounds accept.
const VOLTAGE_CEILING: Voltage = Voltage::from_mv(1675);

/// Bounds of a machine with a fixed and a power tuner.
pub(crate) const BOUNDS: MiningBounds = crate::bounds(
    FREQUENCY,
    VOLTAGE_CEILING,
    &[
        TunerInfo::Fixed { name: "Fixed" },
        TunerInfo::Power {
            name: "PowerRamp",
            reads_efficiency: true,
        },
    ],
    Some(Power::from_mw(19_000)..=Power::from_mw(67_000)),
);

/// Bounds of a machine with a fixed tuner alone.
pub(crate) static FIXED_ONLY_BOUNDS: MiningBounds = crate::bounds(
    FREQUENCY,
    VOLTAGE_CEILING,
    &[TunerInfo::Fixed { name: "Fixed" }],
    None,
);

/// The mining metadata of a machine that accepts `bounds` and starts on
/// the fixed tuning.
pub(crate) const fn info(bounds: &'static MiningBounds) -> MiningInfo {
    crate::mining_info(
        "test chip",
        "test machine",
        "blue",
        4,
        &DEFAULTS,
        bounds,
        DefaultTuning::Fixed,
    )
}

/// The mining metadata of the machine with both tuners.
pub(crate) static INFO: MiningInfo = info(&BOUNDS);

/// The config a first boot builds from [`DEFAULTS`].
pub(crate) fn default_config() -> Config {
    Config::for_board(&DEFAULTS, DefaultTuning::Fixed)
}
