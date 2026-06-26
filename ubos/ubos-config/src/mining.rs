//! Mining tuning configuration, bounds, and static chip metadata.
//!
//! Defines [`TuningConfig`] for ASIC frequency and voltage overrides,
//! [`MiningBounds`] with the accepted ranges, [`MiningInfo`] for the
//! static chip facts that live alongside the config, and the
//! [`BOUNDS`] and [`INFO`] constants.

use core::ops::RangeInclusive;
use ubos_asic::BoardDescriptor;

/// ASIC frequency and voltage overrides.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TuningConfig {
    /// Target ASIC core frequency in MHz.
    pub frequency_mhz: u32,
    /// Core supply voltage in millivolts.
    pub voltage_mv: u32,
}

impl Default for TuningConfig {
    /// Factory defaults for the BMM100 board. The firmware builds its real
    /// per-board defaults via [`crate::Config::for_board`]; this exists so
    /// the test suite has a stable baseline.
    fn default() -> Self {
        Self {
            frequency_mhz: ubos_asic::BMM100.default_frequency_mhz,
            voltage_mv: ubos_asic::BMM100.default_voltage_mv,
        }
    }
}

impl TuningConfig {
    /// Validate frequency and voltage against `bounds`.
    ///
    /// The caller passes the active board's bounds (the firmware sources
    /// them from its `BoardDescriptor` via [`bounds`]) so a board only
    /// accepts an operating point inside its own rail/clock range.
    ///
    /// # Errors
    ///
    /// Returns `Err` when either field falls outside its accepted range.
    pub fn validate(&self, bounds: &MiningBounds) -> Result<(), &'static str> {
        if !bounds.frequency_mhz.contains(&self.frequency_mhz) {
            return Err("tuning.frequency_mhz must be between 50 and 1000");
        }
        if !bounds.voltage_mv.contains(&self.voltage_mv) {
            return Err("tuning.voltage_mv outside the accepted range");
        }
        Ok(())
    }
}

/// Accepted ranges for each [`TuningConfig`] field.
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

/// Accepted value ranges for [`TuningConfig`] on `board`.
///
/// The voltage range is the board's declared tuning policy
/// (`BoardDescriptor::tuning_voltage_*` in `ubos-asic`); on the BM1370
/// hashnode that is the full series-stacked rail span, not a per-chip
/// value. The frequency range is the shared PLL-calculator span. Each
/// firmware crate builds its own `BOUNDS` const from this with its
/// explicit board descriptor.
#[must_use]
pub const fn bounds(board: &'static BoardDescriptor) -> MiningBounds {
    MiningBounds {
        frequency_mhz: 50..=1000,
        voltage_mv: board.tuning_voltage_min_mv..=board.tuning_voltage_max_mv,
    }
}

/// Bounds for the BMM100 board. The firmware uses [`bounds`] with its own
/// descriptor; this exists so the test suite has a stable baseline.
pub const BOUNDS: MiningBounds = bounds(&ubos_asic::BMM100);

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
    /// Accepted tuning ranges.
    pub bounds: &'static MiningBounds,
}

/// Static capability metadata for `board`. The chip model comes from the
/// `ubos-asic` chip descriptor. Each firmware crate builds its
/// own `MINING_INFO` const from this with its explicit board descriptor.
#[must_use]
pub const fn mining_info(
    board: &'static BoardDescriptor,
    bounds: &'static MiningBounds,
) -> MiningInfo {
    MiningInfo {
        asic_model: board.chip.asic_model(),
        device_model: board.model,
        swarm_color: "blue",
        asic_count: ubos_core::consts::Mining::NUM_CHIPS,
        default_frequency_mhz: board.default_frequency_mhz,
        default_voltage_mv: board.default_voltage_mv,
        bounds,
    }
}
