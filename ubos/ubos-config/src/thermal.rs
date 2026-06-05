//! Thermal monitoring configuration.
//!
//! Defines [`ThermalConfig`] for the thermal monitor loop.

/// Tunable parameters for the thermal monitor loop.
///
/// `sample_interval_ms` is stored as milliseconds for serde
/// simplicity; the runtime converts to `Duration` at the boundary.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ThermalConfig {
    /// Poll interval in milliseconds.
    pub sample_interval_ms: u32,
    /// Temperature in Celsius above which mining is halted.
    pub dangerous_temp_c: f32,
    /// Temperature in Celsius below which a halted miner may resume.
    pub cooldown_temp_c: f32,
}

impl Default for ThermalConfig {
    /// Factory defaults: 1-second poll interval, 90 °C halt threshold,
    /// 45 °C resume threshold.
    fn default() -> Self {
        Self {
            sample_interval_ms: 1000,
            dangerous_temp_c: 90.0,
            cooldown_temp_c: 45.0,
        }
    }
}
