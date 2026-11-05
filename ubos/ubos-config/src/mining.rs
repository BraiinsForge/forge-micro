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

//! Mining tuning configuration, bounds, and static chip metadata.
//!
//! Re-exports [`FrequencyVoltageTarget`] for ASIC frequency and voltage
//! overrides and [`PowerTarget`] for a PSU input-power target from the
//! live version of the config schema. Defines [`TunerInfo`] and
//! [`DefaultTuning`] for the tuners a machine offers, [`MiningBounds`]
//! with the accepted ranges, [`MiningInfo`] for the static chip facts
//! that live alongside the config, and the [`BOUNDS`] constant.

use core::ops::RangeInclusive;
use ubos_asic::HashboardDescriptor;
use ubos_core::units::{Efficiency, Frequency, Power, Voltage};

use crate::miner::Tuning;

pub use crate::versions::live::{FrequencyVoltageTarget, PowerTarget};

/// One tuner a machine offers: the tuning kind it reads, the name the
/// mining form shows for it, and the target fields it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all_fields = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TunerInfo {
    /// Reads a fixed tuning, both its frequency and its voltage.
    Fixed { name: &'static str },
    /// Reads a power tuning: its input power, and its efficiency when
    /// `readsEfficiency` is set.
    Power {
        name: &'static str,
        reads_efficiency: bool,
    },
}

impl TunerInfo {
    /// Whether this tuner reads `tuning`'s variant.
    #[must_use]
    pub const fn reads(&self, tuning: &Tuning) -> bool {
        matches!(
            (self, tuning),
            (Self::Fixed { .. }, Tuning::Fixed(_)) | (Self::Power { .. }, Tuning::Power(_))
        )
    }
}

/// The tuning a new config starts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all_fields = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DefaultTuning {
    /// The board's default frequency and voltage.
    Fixed,
    /// The given PSU input power in mW, at the board's default
    /// efficiency.
    Power {
        #[serde(rename = "inputPowerMw")]
        input_power: Power,
    },
}

impl DefaultTuning {
    /// The tuning this default stands for on `board`.
    #[must_use]
    pub const fn tuning(self, board: &HashboardDescriptor) -> Tuning {
        match self {
            Self::Fixed => Tuning::Fixed(FrequencyVoltageTarget {
                frequency: board.default_frequency,
                voltage: board.default_voltage,
            }),
            Self::Power { input_power } => Tuning::Power(PowerTarget {
                input_power,
                efficiency: board.default_efficiency,
            }),
        }
    }
}

/// Accepted ranges for each tuning target field, and the tuners a
/// tuning may select. The API reports them beside the current config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MiningBounds {
    /// Accepted range for the target frequency. The API names it
    /// `frequencyKhz`.
    #[serde(rename = "frequencyKhz")]
    pub frequency: RangeInclusive<Frequency>,
    /// Accepted range for the target voltage. The API names it
    /// `voltageMv`.
    #[serde(rename = "voltageMv")]
    pub voltage: RangeInclusive<Voltage>,
    /// Accepted range for the target input power. Present exactly when
    /// one of `tuners` reads a power target. The API names it
    /// `inputPowerMw`.
    #[serde(rename = "inputPowerMw")]
    pub input_power: Option<RangeInclusive<Power>>,
    /// The tuners the machine offers, in the order the mining form
    /// shows them.
    pub tuners: &'static [TunerInfo],
}

impl MiningBounds {
    /// Whether a power target may ask for `input_power`.
    #[must_use]
    pub const fn accepts_input_power(&self, input_power: Power) -> bool {
        match &self.input_power {
            Some(range) => {
                range.start().mw() <= input_power.mw() && input_power.mw() <= range.end().mw()
            }
            None => true,
        }
    }
}

/// Accepted value ranges for a [`Tuning`] on `board`, and the machine's
/// `tuners`.
///
/// The frequency range is the chip's own, which the fixed tuner clamps
/// to. The voltage ceiling is the board's open-core rail voltage
/// (`HashboardDescriptor::max_rail_voltage`: max chip voltage times the
/// number of series domains, capped by the PSU). There is no lower
/// limit, so the voltage floor is zero. No descriptor states what the
/// machine may draw, so the firmware states `input_power` itself.
/// Each firmware crate builds its own `BOUNDS` const from this with its
/// explicit board descriptor.
///
/// # Panics
///
/// When `input_power` is given and no tuner reads a power target, or
/// a tuner reads one and it is not given, which fails the build of the
/// `BOUNDS` const.
#[must_use]
pub const fn bounds(
    board: &'static HashboardDescriptor,
    tuners: &'static [TunerInfo],
    input_power: Option<RangeInclusive<Power>>,
) -> MiningBounds {
    let mut index = 0;
    while index < tuners.len() && !matches!(tuners[index], TunerInfo::Power { .. }) {
        index += 1;
    }
    assert!(
        (index < tuners.len()) == input_power.is_some(),
        "BUG: an input-power range is given exactly when a tuner reads a power target"
    );
    MiningBounds {
        frequency: board.chip.frequency_range.min()..=board.chip.frequency_range.max(),
        voltage: Voltage::from_mv(0)..=board.max_rail_voltage(),
        input_power,
        tuners,
    }
}

/// Bounds for the BMM101 board with a fixed and a power tuner. The
/// firmware uses [`bounds`] with its own descriptor and tuners; this
/// exists so the test suite has a stable baseline.
pub const BOUNDS: MiningBounds = bounds(
    &ubos_asic::BMM101_HASHBOARD,
    &[
        TunerInfo::Fixed { name: "Fixed" },
        TunerInfo::Power {
            name: "PowerRamp",
            reads_efficiency: true,
        },
    ],
    Some(Power::from_mw(19_000)..=Power::from_mw(67_000)),
);

/// Static capability metadata for the ASIC and board.
///
/// Groups the chip-specific constants (model, board name, swarm color,
/// chip count) with the tuning defaults and bounds so the REST API can
/// serve them from a single serializable value without touching runtime
/// state.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MiningInfo {
    /// ASIC chip model name.
    pub asic_model: &'static str,
    /// Board product name reported to external clients.
    pub device_model: &'static str,
    /// Swarm color assigned to this device type for UI grouping.
    pub swarm_color: &'static str,
    /// Number of ASIC chips on the hashboard.
    pub asic_count: u8,
    /// Default mining frequency. The API names it
    /// `defaultFrequencyKhz`.
    #[serde(rename = "defaultFrequencyKhz")]
    pub default_frequency: Frequency,
    /// Default core voltage. The API names it `defaultVoltageMv`.
    #[serde(rename = "defaultVoltageMv")]
    pub default_voltage: Voltage,
    /// Default power-target efficiency. The API names it
    /// `defaultEfficiencyMjPerTh`.
    #[serde(rename = "defaultEfficiencyMjPerTh")]
    pub default_efficiency: Efficiency,
    /// The tuning a new config starts on.
    pub default_tuning: DefaultTuning,
    /// Accepted tuning ranges.
    pub bounds: &'static MiningBounds,
}

/// Static capability metadata for `board`. The chip model comes from the
/// `ubos-asic` chip descriptor. Each firmware crate builds its
/// own `MINING_INFO` const from this with its explicit board descriptor.
///
/// # Panics
///
/// When no tuner in `bounds` reads `default_tuning`, or its input power
/// is outside the accepted range, which fails the build of the
/// `MINING_INFO` const.
#[must_use]
pub const fn mining_info(
    board: &'static HashboardDescriptor,
    bounds: &'static MiningBounds,
    default_tuning: DefaultTuning,
) -> MiningInfo {
    let tuning = default_tuning.tuning(board);
    let mut index = 0;
    while index < bounds.tuners.len() && !bounds.tuners[index].reads(&tuning) {
        index += 1;
    }
    assert!(
        index < bounds.tuners.len(),
        "BUG: no tuner reads the default tuning"
    );
    if let DefaultTuning::Power { input_power } = default_tuning {
        assert!(
            bounds.accepts_input_power(input_power),
            "BUG: the default input power is outside the accepted range"
        );
    }
    MiningInfo {
        asic_model: board.chip.asic_model,
        device_model: board.model,
        swarm_color: board.swarm_color,
        asic_count: board.num_chips,
        default_frequency: board.default_frequency,
        default_voltage: board.default_voltage,
        default_efficiency: board.default_efficiency,
        default_tuning,
        bounds,
    }
}
