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
//! Defines [`FrequencyVoltageTarget`] for ASIC frequency and voltage overrides,
//! [`PowerTarget`] for a PSU input-power target,
//! [`MiningBounds`] with the accepted ranges, [`MiningInfo`] for the
//! static chip facts that live alongside the config, and the
//! [`BOUNDS`] constant.

use core::num::NonZeroU32;
use core::ops::RangeInclusive;
use ubos_asic::HashboardDescriptor;

/// ASIC frequency and voltage overrides.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FrequencyVoltageTarget {
    /// Target ASIC core frequency in MHz.
    pub frequency_mhz: u32,
    /// Core supply voltage in millivolts.
    pub voltage_mv: u32,
}

/// The power a power tuner holds the machine at.
///
/// PSU input power, so it is the figure a site meters and bills, not
/// the hashboard draw. What range a machine accepts follows from its
/// supply and its boards, and the tuner that reads this is the only
/// thing that knows it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PowerTarget {
    /// Target PSU input power in mW.
    pub input_power_mw: u32,
    /// Target input power per theoretical hashrate in mJ/TH, which caps
    /// the clock the tuner ramps to.
    pub efficiency_mj_per_th: NonZeroU32,
}

/// Accepted ranges for each [`FrequencyVoltageTarget`] field.
///
/// Serializable so API endpoints can return the bounds alongside the
/// current config without a separate hard-coded schema.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MiningBounds {
    /// Accepted range for `frequency_mhz` in MHz.
    pub frequency_mhz: RangeInclusive<u32>,
    /// Accepted range for `voltage_mv` in millivolts.
    pub voltage_mv: RangeInclusive<u32>,
}

/// Accepted value ranges for [`FrequencyVoltageTarget`] on `board`.
///
/// The voltage ceiling is the board's open-core rail voltage
/// (`HashboardDescriptor::max_rail_voltage`: max chip voltage times the
/// number of series domains, capped by the PSU). There is no lower
/// limit, so the range floor is zero. The frequency range is the shared
/// PLL-calculator span. Each firmware crate builds its own `BOUNDS`
/// const from this with its explicit board descriptor.
#[must_use]
pub const fn bounds(board: &'static HashboardDescriptor) -> MiningBounds {
    MiningBounds {
        frequency_mhz: 50..=1000,
        voltage_mv: 0..=board.max_rail_voltage().mv(),
    }
}

/// Bounds for the BMM101 board. The firmware uses [`bounds`] with its
/// own descriptor; this exists so the test suite has a stable baseline.
pub const BOUNDS: MiningBounds = bounds(&ubos_asic::BMM101_HASHBOARD);

/// Static capability metadata for the ASIC and board.
///
/// Groups the chip-specific constants (model, board name, swarm color,
/// chip count) with the tuning defaults and bounds so the REST API can
/// serve them from a single serializable value without touching runtime
/// state.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MiningInfo {
    /// ASIC chip model name.
    pub asic_model: &'static str,
    /// Board product name reported to external clients.
    pub device_model: &'static str,
    /// Swarm color assigned to this device type for UI grouping.
    pub swarm_color: &'static str,
    /// Number of ASIC chips on the hashboard.
    pub asic_count: u8,
    /// Default mining frequency in MHz.
    pub default_frequency_mhz: u32,
    /// Default core voltage in millivolts.
    pub default_voltage_mv: u32,
    /// Default power-target efficiency in mJ/TH.
    pub default_efficiency_mj_per_th: u32,
    /// Accepted tuning ranges.
    pub bounds: &'static MiningBounds,
}

/// Static capability metadata for `board`. The chip model comes from the
/// `ubos-asic` chip descriptor. Each firmware crate builds its
/// own `MINING_INFO` const from this with its explicit board descriptor.
#[must_use]
pub const fn mining_info(
    board: &'static HashboardDescriptor,
    bounds: &'static MiningBounds,
) -> MiningInfo {
    MiningInfo {
        asic_model: board.chip.asic_model,
        device_model: board.model,
        swarm_color: board.swarm_color,
        asic_count: board.num_chips,
        default_frequency_mhz: board.default_frequency.mhz(),
        default_voltage_mv: board.default_voltage.mv(),
        default_efficiency_mj_per_th: board.default_efficiency.mj_per_th(),
        bounds,
    }
}
