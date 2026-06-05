//! Thermal monitoring configuration and validation.
//!
//! Defines [`ThermalConfig`] for the thermal monitor loop,
//! [`ThermalBounds`] for the accepted ranges, and the static
//! [`BOUNDS`] constant.

use core::ops::RangeInclusive;

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

impl ThermalConfig {
    /// Validate all fields against [`BOUNDS`].
    ///
    /// Returns the first error found as a static operator-readable string.
    ///
    /// # Errors
    ///
    /// Returns `Err` when any field is out of range or the ordering
    /// invariant `cooldown_temp_c < dangerous_temp_c` is violated.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !BOUNDS.sample_interval_ms.contains(&self.sample_interval_ms) {
            return Err("thermal.sample_interval_ms must be between 100 and 60000");
        }
        if !self.dangerous_temp_c.is_finite() {
            return Err("thermal.dangerous_temp_c must be a finite number");
        }
        if !BOUNDS.dangerous_temp_c.contains(&self.dangerous_temp_c) {
            return Err("thermal.dangerous_temp_c must be between 50 and 110");
        }
        if !self.cooldown_temp_c.is_finite() {
            return Err("thermal.cooldown_temp_c must be a finite number");
        }
        if !BOUNDS.cooldown_temp_c.contains(&self.cooldown_temp_c) {
            return Err("thermal.cooldown_temp_c must be between 20 and 90");
        }
        if self.cooldown_temp_c >= self.dangerous_temp_c {
            return Err("thermal.cooldown_temp_c must be less than thermal.dangerous_temp_c");
        }
        Ok(())
    }
}

/// Accepted ranges for each [`ThermalConfig`] field.
///
/// Serializable so API endpoints can return the bounds alongside the
/// current config without a separate hard-coded schema.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ThermalBounds {
    /// Accepted range for `sample_interval_ms` in milliseconds.
    pub sample_interval_ms: RangeInclusive<u32>,
    /// Accepted range for `dangerous_temp_c` in Celsius.
    pub dangerous_temp_c: RangeInclusive<f32>,
    /// Accepted range for `cooldown_temp_c` in Celsius.
    pub cooldown_temp_c: RangeInclusive<f32>,
}

/// Accepted value ranges for [`ThermalConfig`] fields.
pub const BOUNDS: ThermalBounds = ThermalBounds {
    sample_interval_ms: 100..=60_000,
    dangerous_temp_c: 50.0..=110.0,
    cooldown_temp_c: 20.0..=90.0,
};
