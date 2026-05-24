//! Response shape for `GET /api/system/asic`.
//!
//! Lists capability metadata for the chip. `asicModel` is a free-form
//! string rather than the spec's fixed enum so future silicon ships
//! without a schema bump. `frequencyOptions` and `voltageOptions`
//! advertise what the firmware will accept on a future PATCH.

use serde::Serialize;

/// Default mining frequency in MHz programmed at chip init.
pub const DEFAULT_FREQUENCY_MHZ: u32 = 525;

/// Discrete frequencies in MHz that the chip and firmware support.
///
/// Used to populate `frequencyOptions` in `SystemAsic` and to
/// validate future PATCH requests.
pub const FREQUENCY_OPTIONS_MHZ: &[u32] = &[400, 425, 450, 475, 500, 525, 550, 575, 600];

/// Discrete core voltages in millivolts that the PSU driver supports.
///
/// Used to populate `voltageOptions` in `SystemAsic` and to
/// validate future PATCH requests.
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
