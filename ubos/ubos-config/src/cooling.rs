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

//! Cooling loop configuration and validation.
//!
//! Defines [`CoolingConfig`], [`PidConfig`], [`CoolingBounds`], and
//! the static [`BOUNDS`] constant.

use core::ops::RangeInclusive;

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

impl CoolingConfig {
    /// Validate the proposed `target` temperature against [`BOUNDS`] and
    /// `dangerous_temp_c`.
    ///
    /// `dangerous_temp_c` is resolved by the caller to either the value
    /// being set in the same update or the current live config's value.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `target` is non-finite, outside
    /// `BOUNDS.target_temp_c`, or not strictly less than
    /// `dangerous_temp_c`.
    pub fn validate_target_temp(target: f32, dangerous_temp_c: f32) -> Result<(), &'static str> {
        if !target.is_finite() {
            return Err("cooling.target_temp_c must be a finite number");
        }
        if !BOUNDS.target_temp_c.contains(&target) {
            return Err("cooling.target_temp_c must be between 30 and 85");
        }
        if target >= dangerous_temp_c {
            return Err("cooling.target_temp_c must be less than dangerous_temp_c");
        }
        Ok(())
    }
}

/// Accepted ranges for each [`CoolingConfig`] field.
///
/// Serializable so API endpoints can return the bounds alongside the
/// current config without a separate hard-coded schema.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoolingBounds {
    /// Accepted range for `target_temp_c` in Celsius.
    pub target_temp_c: RangeInclusive<f32>,
}

/// Accepted value ranges for [`CoolingConfig`] fields.
pub const BOUNDS: CoolingBounds = CoolingBounds {
    target_temp_c: 30.0..=85.0,
};

/// Static bounds metadata for the cooling config section.
///
/// Carries the accepted ranges for [`CoolingConfig`] so the webui can
/// pull input `min`/`max` attributes from the same source the firmware
/// validates against.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoolingInfo {
    /// Accepted ranges for [`CoolingConfig`] fields.
    pub bounds: &'static CoolingBounds,
}

/// Bounds metadata served at `GET /api/system/cooling`.
pub const INFO: CoolingInfo = CoolingInfo { bounds: &BOUNDS };
