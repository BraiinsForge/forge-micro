//! Cooling loop configuration.
//!
//! Defines [`CoolingConfig`] and [`PidConfig`].

/// PID controller gains for the cooling loop.
///
/// Separated from [`CoolingConfig`] so gains can be retuned without
/// touching the thermal setpoint.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PidConfig {
    /// Minimum PWM duty cycle; prevents the fan from stopping entirely.
    pub min_fan_pwm: u8,
    /// Proportional gain.
    pub p_gain: f32,
    /// Integral gain.
    pub i_gain: f32,
    /// Derivative gain.
    pub d_gain: f32,
    /// Half-decay time for the derivative smoother in seconds.
    pub smoother_half_decay_secs: f32,
}

impl Default for PidConfig {
    /// Factory-tuned PID gains for the BMM101 cooling profile.
    fn default() -> Self {
        Self {
            min_fan_pwm: 20,
            p_gain: -5.0,
            i_gain: -0.15,
            d_gain: -100.0,
            smoother_half_decay_secs: 10.0,
        }
    }
}

/// Tunable parameters for the cooling loop.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoolingConfig {
    /// PID setpoint temperature in Celsius.
    pub target_temp_c: f32,
    /// PID controller gains.
    pub pid: PidConfig,
}

impl Default for CoolingConfig {
    /// Factory default: 67 °C setpoint with tuned PID gains.
    fn default() -> Self {
        Self {
            target_temp_c: 67.0,
            pid: PidConfig::default(),
        }
    }
}
