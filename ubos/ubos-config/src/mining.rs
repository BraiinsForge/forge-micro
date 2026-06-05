//! Mining tuning configuration.
//!
//! Defines [`TuningConfig`] for ASIC frequency and voltage overrides.

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
