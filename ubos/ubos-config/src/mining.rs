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
//! with the accepted ranges, and [`MiningInfo`] for the static chip
//! facts that live alongside the config.

use core::ops::RangeInclusive;
use ubos_core::units::{Efficiency, Frequency, Power, Voltage};

use crate::board::HashboardDefaults;
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
    /// The tuning this default stands for on a board that hands in
    /// `defaults`.
    #[must_use]
    pub const fn tuning(self, defaults: &HashboardDefaults) -> Tuning {
        match self {
            Self::Fixed => Tuning::Fixed(FrequencyVoltageTarget {
                frequency: defaults.frequency,
                voltage: defaults.voltage,
            }),
            Self::Power { input_power } => Tuning::Power(PowerTarget {
                input_power,
                efficiency: defaults.efficiency,
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

/// Accepted value ranges for a [`Tuning`], and the machine's `tuners`.
///
/// `frequency` is the range of the chip, which the fixed tuner clamps
/// to. `voltage_ceiling` is the highest rail voltage the board may
/// carry, and the floor is zero. The firmware states `input_power`
/// itself, because it knows what the machine may draw. The crate that
/// holds the hardware numbers of a board calls this with them, and each
/// firmware crate builds its own `BOUNDS` const through that crate.
///
/// # Panics
///
/// When `input_power` is given and no tuner reads a power target, or a
/// tuner reads one and it is not given.
#[must_use]
pub const fn bounds(
    frequency: RangeInclusive<Frequency>,
    voltage_ceiling: Voltage,
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
        frequency,
        voltage: Voltage::from_mv(0)..=voltage_ceiling,
        input_power,
        tuners,
    }
}

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

/// Static capability metadata for a board: its chip model, its product
/// name, its swarm color, its chip count and the `defaults` it hands to
/// a new config. The crate that holds the hardware facts of a board
/// calls this with them, and each firmware crate builds its own
/// `MINING_INFO` const through that crate.
///
/// # Panics
///
/// When no tuner in `bounds` reads `default_tuning`, or its input power is
/// outside the accepted range.
#[must_use]
pub const fn mining_info(
    asic_model: &'static str,
    device_model: &'static str,
    swarm_color: &'static str,
    asic_count: u8,
    defaults: &HashboardDefaults,
    bounds: &'static MiningBounds,
    default_tuning: DefaultTuning,
) -> MiningInfo {
    let tuning = default_tuning.tuning(defaults);
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
        asic_model,
        device_model,
        swarm_color,
        asic_count,
        default_frequency: defaults.frequency,
        default_voltage: defaults.voltage,
        default_efficiency: defaults.efficiency,
        default_tuning,
        bounds,
    }
}
