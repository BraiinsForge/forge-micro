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
//! [`TunerInfo`] and [`DefaultTuning`] for the tuners a machine offers,
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

/// The tuning a new config starts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all_fields = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DefaultTuning {
    /// The board's default frequency and voltage.
    Fixed,
    /// `input_power_mw` of PSU input power, at the board's default
    /// efficiency.
    Power { input_power_mw: u32 },
}

impl DefaultTuning {
    /// The tuning this default stands for on `board`.
    #[must_use]
    pub const fn tuning(self, board: &HashboardDescriptor) -> Tuning {
        match self {
            Self::Fixed => Tuning::Fixed(FrequencyVoltageTarget {
                frequency_khz: board.default_frequency.khz(),
                voltage_mv: board.default_voltage.mv(),
            }),
            Self::Power { input_power_mw } => Tuning::Power(PowerTarget {
                input_power_mw,
                efficiency_mj_per_th: NonZeroU32::new(board.default_efficiency.mj_per_th())
                    .expect("BUG: efficiency is zero"),
            }),
        }
    }
}

/// Accepted ranges for each [`Tuning`] field, and the tuners a
/// [`Tuning`] may select.
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
    /// Accepted range for `input_power_mw` in mW. Present exactly when
    /// one of `tuners` reads a power target.
    pub input_power_mw: Option<RangeInclusive<u32>>,
    /// The tuners the machine offers, in the order the mining form
    /// shows them.
    pub tuners: &'static [TunerInfo],
}

impl MiningBounds {
    /// Whether a power target may ask for `input_power_mw`.
    #[must_use]
    pub const fn accepts_input_power(&self, input_power_mw: u32) -> bool {
        match &self.input_power_mw {
            Some(range) => *range.start() <= input_power_mw && input_power_mw <= *range.end(),
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
/// machine may draw, so the firmware states `input_power_mw` itself.
/// Each firmware crate builds its own `BOUNDS` const from this with its
/// explicit board descriptor.
///
/// # Panics
///
/// When `input_power_mw` is given and no tuner reads a power target, or
/// a tuner reads one and it is not given, which fails the build of the
/// `BOUNDS` const.
#[must_use]
pub const fn bounds(
    board: &'static HashboardDescriptor,
    tuners: &'static [TunerInfo],
    input_power_mw: Option<RangeInclusive<u32>>,
) -> MiningBounds {
    let mut index = 0;
    while index < tuners.len() && !matches!(tuners[index], TunerInfo::Power { .. }) {
        index += 1;
    }
    assert!(
        (index < tuners.len()) == input_power_mw.is_some(),
        "BUG: an input-power range is given exactly when a tuner reads a power target"
    );
    MiningBounds {
        frequency_khz: board.chip.frequency_range.min().khz()
            ..=board.chip.frequency_range.max().khz(),
        voltage_mv: 0..=board.max_rail_voltage().mv(),
        input_power_mw,
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
    Some(19_000..=67_000),
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
    if let DefaultTuning::Power { input_power_mw } = default_tuning {
        assert!(
            bounds.accepts_input_power(input_power_mw),
            "BUG: the default input power is outside the accepted range"
        );
    }
    MiningInfo {
        asic_model: board.chip.asic_model,
        device_model: board.model,
        swarm_color: board.swarm_color,
        asic_count: board.num_chips,
        default_frequency_khz: board.default_frequency.khz(),
        default_voltage_mv: board.default_voltage.mv(),
        default_efficiency_mj_per_th: board.default_efficiency.mj_per_th(),
        default_tuning,
        bounds,
    }
}
