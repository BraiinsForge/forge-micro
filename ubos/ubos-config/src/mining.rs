//! Mining tuning configuration, bounds, and static chip metadata.
//!
//! Defines [`TuningConfig`] for ASIC frequency and voltage overrides,
//! [`MiningBounds`] with the accepted ranges, [`MiningInfo`] for the
//! static chip facts that live alongside the config, and the
//! [`BOUNDS`] and [`INFO`] constants.

use core::ops::RangeInclusive;
use ubos_asic::ACTIVE_BOARD as BOARD;

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
    /// Factory defaults from the active board descriptor, so the values
    /// always fall inside [`BOUNDS`] regardless of the selected board.
    fn default() -> Self {
        Self {
            frequency_mhz: BOARD.default_frequency_mhz,
            voltage_mv: BOARD.default_voltage_mv,
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
            return Err("tuning.frequency_mhz must be between 50 and 1000");
        }
        if !BOUNDS.voltage_mv.contains(&self.voltage_mv) {
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

/// Accepted value ranges for [`TuningConfig`] fields.
///
/// The voltage range is the active board's declared tuning policy
/// (`BoardDescriptor::tuning_voltage_*` in `ubos-asic`); on the BM1370
/// hashnode that is the full series-stacked rail span, not a per-chip
/// value. The frequency range is the shared PLL-calculator span.
pub const BOUNDS: MiningBounds = MiningBounds {
    frequency_mhz: 50..=1000,
    voltage_mv: BOARD.tuning_voltage_min_mv..=BOARD.tuning_voltage_max_mv,
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

/// Compile-time decimal parse of an `option_env!` value, with a fallback.
/// Mirrors the build-time override `Config::default()` applies at runtime
/// so the reported defaults match what the board actually boots with
/// (e.g. the hashnode's 500 MHz / 4500 mV, not the per-chip baseline).
const fn env_u32(value: Option<&str>, default: u32) -> u32 {
    let Some(text) = value else {
        return default;
    };
    let bytes = text.as_bytes();
    let mut result: u32 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let digit = bytes[index];
        assert!(
            digit.is_ascii_digit(),
            "env override must be decimal digits"
        );
        // Overflow here is a compile-time error because const evaluation catches
        // wrapping arithmetic before the binary is produced.
        result = result * 10 + (digit - b'0') as u32;
        index += 1;
    }
    result
}

/// Static capability metadata for the active board. The chip model comes
/// from the `ubos-asic` chip descriptor so it tracks the real silicon; the
/// default frequency/voltage mirror the build-time env overrides so the
/// dashboard reports the board's real operating point.
pub const INFO: MiningInfo = MiningInfo {
    asic_model: BOARD.chip.asic_model(),
    device_model: BOARD.model,
    swarm_color: "blue",
    asic_count: ubos_core::consts::Mining::NUM_CHIPS,
    default_frequency_mhz: env_u32(
        option_env!("MINING_FREQUENCY_MHZ"),
        BOARD.default_frequency_mhz,
    ),
    default_voltage_mv: env_u32(option_env!("MINING_VOLTAGE_MV"), BOARD.default_voltage_mv),
    bounds: &BOUNDS,
};
