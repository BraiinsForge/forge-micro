//! Mining tuning configuration, bounds, and static chip metadata.
//!
//! Defines [`TuningConfig`] for ASIC frequency and voltage overrides,
//! [`MiningBounds`] with the accepted ranges, [`MiningInfo`] for the
//! static chip facts that live alongside the config, and the
//! [`BOUNDS`] and [`INFO`] constants.

use core::ops::RangeInclusive;

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
    /// Factory defaults derived from the ubos-core mining constants.
    fn default() -> Self {
        Self {
            frequency_mhz: ubos_core::consts::Mining::DEFAULT_FREQUENCY.mhz(),
            voltage_mv: ubos_core::consts::Mining::DEFAULT_VOLTAGE.mv(),
        }
    }
}

impl TuningConfig {
    /// Validate frequency and voltage against [`BOUNDS`].
    ///
    /// # Errors
    ///
    /// Returns `Err` when either field falls outside its accepted range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !BOUNDS.frequency_mhz.contains(&self.frequency_mhz) {
            return Err("tuning.frequency_mhz must be between 400 and 600");
        }
        if !BOUNDS.voltage_mv.contains(&self.voltage_mv) {
            return Err("tuning.voltage_mv must be between 1100 and 1500");
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

/// Accepted value ranges for [`TuningConfig`] fields.
pub const BOUNDS: MiningBounds = MiningBounds {
    frequency_mhz: 400..=600,
    voltage_mv: 1100..=1500,
};

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

/// Static capability metadata for the BM1362-based hashboard.
pub const INFO: MiningInfo = MiningInfo {
    asic_model: "BM1362",
    device_model: "miniminer",
    swarm_color: "blue",
    asic_count: ubos_mining::consts::NUM_CHIPS,
    default_frequency_mhz: ubos_core::consts::Mining::DEFAULT_FREQUENCY.mhz(),
    default_voltage_mv: ubos_core::consts::Mining::DEFAULT_VOLTAGE.mv(),
    bounds: &BOUNDS,
};
