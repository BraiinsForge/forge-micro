//! Response shape for `GET /api/system/asic`.
//!
//! Lists capability metadata for the chip. `asicModel` is a free-form
//! string rather than the spec's fixed enum so future silicon ships
//! without a schema bump. `frequencyOptions` and `voltageOptions`
//! advertise what the firmware will accept on a future PATCH.

use serde::Serialize;
use ubos_core::consts::Mining;
use ubos_core::units::Frequency;

/// Default mining frequency programmed at chip init. Aliased from
/// `ubos_core::consts::Mining::DEFAULT_FREQUENCY` so the boot-default
/// frequency has one source of truth shared with
/// `ubos-config::Config::default()` and `ubos-mining`.
pub const DEFAULT_FREQUENCY: Frequency = Mining::DEFAULT_FREQUENCY;

/// Discrete frequencies in MHz for the JSON wire representation.
///
/// `SystemAsic` serializes `frequency_options` as `&'static [u32]`;
/// this slice feeds that field.
pub const FREQUENCY_OPTIONS_MHZ: &[u32] = &[400, 425, 450, 475, 500, 525, 550, 575, 600];

/// Discrete core voltages in millivolts for the JSON wire representation.
///
/// `SystemAsic` serializes `voltage_options` as `&'static [u32]`;
/// this slice feeds that field.
pub const VOLTAGE_OPTIONS_MV: &[u32] = &[1100, 1150, 1200, 1250, 1300, 1350, 1400, 1450, 1500];

/// ASIC capability metadata for `GET /api/system/asic`.
///
/// All fields are static; this struct can be constructed as a
/// `const` and serialized on demand without touching any run-time
/// state.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemAsic {
    /// ASIC chip model name; free-form string so future chips ship
    /// without a schema version bump.
    pub asic_model: &'static str,
    /// Board product name reported to external clients.
    pub device_model: &'static str,
    /// Swarm color assigned to this device type for UI grouping.
    pub swarm_color: &'static str,
    /// Number of ASIC chips on the hashboard.
    pub asic_count: u8,
    /// Default mining frequency in MHz.
    pub default_frequency: u32,
    /// Supported frequency values in MHz.
    pub frequency_options: &'static [u32],
    /// Default core voltage in millivolts.
    pub default_voltage: u32,
    /// Supported core voltage values in millivolts.
    pub voltage_options: &'static [u32],
}
