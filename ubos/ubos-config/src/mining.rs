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
//! [`TunerInfo`] for the tuners a machine offers,
//! [`MiningBounds`] with the accepted ranges, [`MiningInfo`] for the
//! static chip facts that live alongside the config, and the
//! [`BOUNDS`] constant.

use core::num::NonZeroU32;
use core::ops::RangeInclusive;
use ubos_asic::HashboardDescriptor;

use crate::miner::Tuning;

/// ASIC frequency and voltage overrides.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FrequencyVoltageTarget {
    /// Target ASIC core frequency in kHz. The PLL grid steps by a
    /// fraction of a megahertz, so a megahertz field cannot name every
    /// point on it.
    pub frequency_khz: u32,
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

/// One tuner a machine offers: the [`Tuning`] variant it reads, the
/// name the mining form shows for it, and the target fields it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all_fields = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TunerInfo {
    /// Reads [`Tuning::Fixed`], both its frequency and its voltage.
    Fixed { name: &'static str },
    /// Reads [`Tuning::Power`]: its input power, and its efficiency when
    /// `reads_efficiency` is set.
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

/// Accepted ranges for each [`FrequencyVoltageTarget`] field, and the
/// tuners a [`Tuning`] may select.
///
/// Serializable so API endpoints can return the bounds alongside the
/// current config without a separate hard-coded schema.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MiningBounds {
    /// Accepted range for `frequency_khz` in kHz.
    pub frequency_khz: RangeInclusive<u32>,
    /// Accepted range for `voltage_mv` in millivolts.
    pub voltage_mv: RangeInclusive<u32>,
    /// The tuners the machine offers, in the order the mining form
    /// shows them.
    pub tuners: &'static [TunerInfo],
}

/// Accepted value ranges for [`FrequencyVoltageTarget`] on `board`, and
/// the machine's `tuners`.
///
/// The voltage ceiling is the board's open-core rail voltage
/// (`HashboardDescriptor::max_rail_voltage`: max chip voltage times the
/// number of series domains, capped by the PSU). There is no lower
/// limit, so the range floor is zero. The frequency range is the shared
/// PLL-calculator span. Each firmware crate builds its own `BOUNDS`
/// const from this with its explicit board descriptor.
#[must_use]
pub const fn bounds(
    board: &'static HashboardDescriptor,
    tuners: &'static [TunerInfo],
) -> MiningBounds {
    MiningBounds {
        frequency_khz: 50_000..=1_000_000,
        voltage_mv: 0..=board.max_rail_voltage().mv(),
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
            name: "Power",
            reads_efficiency: true,
        },
    ],
);

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
    /// Default mining frequency in kHz.
    pub default_frequency_khz: u32,
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
        default_frequency_khz: board.default_frequency.khz(),
        default_voltage_mv: board.default_voltage.mv(),
        default_efficiency_mj_per_th: board.default_efficiency.mj_per_th(),
        bounds,
    }
}
