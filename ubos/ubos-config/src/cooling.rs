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
//! Defines [`CoolingConfig`], [`FanMode`], [`PidConfig`],
//! [`CoolingBounds`], and the static [`BOUNDS`] constant.

use core::ops::RangeInclusive;

/// How the cooling loop decides the fan duty.
///
/// [`FanMode::Auto`] runs the PID against [`CoolingConfig::target_temp_c`];
/// [`FanMode::Fixed`] holds the fan at [`CoolingConfig::fixed_fan_pwm`] and
/// ignores the temperature. Neither mode disables the cooling failsafes: a
/// sensor fault and a stalled thermal task force the fan to full in both.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FanMode {
    /// PID control against the thermal setpoint.
    Auto,
    /// Operator-selected constant duty.
    Fixed,
}

impl FanMode {
    /// Wire name of the variant, for log lines that must read the same
    /// under both logging backends.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed => "fixed",
        }
    }
}

/// PID controller gains for the cooling loop.
///
/// Separated from [`CoolingConfig`] so gains can be retuned without
/// touching the thermal setpoint.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PidConfig {
    /// Floor on the PID's output duty; keeps the PID from stopping the fan.
    /// Bounds nothing outside [`FanMode::Auto`]: in [`FanMode::Fixed`] the
    /// PID is not running and [`CoolingConfig::fixed_fan_pwm`] is held as
    /// given.
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
    /// Whether the loop runs the PID or holds a fixed duty.
    pub mode: FanMode,
    /// PID setpoint temperature in Celsius. Ignored in [`FanMode::Fixed`].
    pub target_temp_c: f32,
    /// Duty cycle held in [`FanMode::Fixed`], 0-100. Not floored by
    /// [`PidConfig::min_fan_pwm`]: the floor is a bound on PID output, and a
    /// fixed duty is what the operator asked for. A board whose hardware
    /// needs a minimum duty clamps it in its own `FanOutput`.
    pub fixed_fan_pwm: u8,
    /// PID controller gains.
    pub pid: PidConfig,
}

impl Default for CoolingConfig {
    /// Factory default: PID control at a 67 °C setpoint with tuned gains.
    fn default() -> Self {
        Self {
            mode: FanMode::Auto,
            target_temp_c: 67.0,
            fixed_fan_pwm: DEFAULT_FIXED_FAN_PWM,
            pid: PidConfig::default(),
        }
    }
}

impl CoolingConfig {
    /// Validate the proposed `target` temperature against [`BOUNDS`] and
    /// `dangerous_estimated_temp_c`.
    ///
    /// `dangerous_estimated_temp_c` is resolved by the caller to either the
    /// value being set in the same update or the current live config's
    /// value. It is the lower of the two halt thresholds, so a target below
    /// it stays below both.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `target` is non-finite, outside
    /// `BOUNDS.target_temp_c`, or not strictly less than
    /// `dangerous_estimated_temp_c`.
    pub fn validate_target_temp(
        target: f32,
        dangerous_estimated_temp_c: f32,
    ) -> Result<(), &'static str> {
        if !target.is_finite() {
            return Err("cooling.target_temp_c must be a finite number");
        }
        if !BOUNDS.target_temp_c.contains(&target) {
            return Err("cooling.target_temp_c must be between 30 and 85");
        }
        if target >= dangerous_estimated_temp_c {
            return Err("cooling.target_temp_c must be less than dangerous_estimated_temp_c");
        }
        Ok(())
    }

    /// Validate the proposed fixed duty.
    ///
    /// The whole 0-100 range is accepted, including 0: a stopped fan is a
    /// legitimate request, and the board stays protected by the mining halt
    /// at the thermal config's dangerous thresholds.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `pwm` falls outside `BOUNDS.fixed_fan_pwm` and so
    /// is not a duty cycle.
    pub fn validate_fixed_fan_pwm(pwm: u8) -> Result<(), &'static str> {
        if !BOUNDS.fixed_fan_pwm.contains(&pwm) {
            return Err("cooling.fixed_fan_pwm must be between 0 and 100");
        }
        Ok(())
    }

    /// Validate the proposed PID output floor.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `pwm` is outside `BOUNDS.min_fan_pwm`. PID output
    /// clamps to `[min_fan_pwm, 100]`, so a floor above 100 would invert the
    /// clamp range; see `ubos-cooling::pid::set_limits`. `TempControl::new`
    /// clamps the floor to 100 before it reaches the controller, so an
    /// unvalidated value cannot invert the range at runtime; rejecting it
    /// here is what keeps a value the operator can see from being silently
    /// altered.
    pub fn validate_min_fan_pwm(pwm: u8) -> Result<(), &'static str> {
        if !BOUNDS.min_fan_pwm.contains(&pwm) {
            return Err("cooling.min_fan_pwm must be between 0 and 100");
        }
        Ok(())
    }
}

/// Duty a board starts with in [`FanMode::Fixed`] before the operator picks
/// one. Never commanded while the default [`FanMode::Auto`] holds; it exists
/// so the UI has a value to show when the mode is first switched.
pub(crate) const DEFAULT_FIXED_FAN_PWM: u8 = 50;

/// Accepted ranges for every numeric [`CoolingConfig`] field.
///
/// `mode` carries no range: it is an enum, and serde rejects any variant
/// outside it. The [`CoolingConfig`] validators check against these ranges
/// rather than against literals of their own, so this is the single
/// definition of what the firmware accepts.
///
/// Serializable so API endpoints can return the bounds alongside the
/// current config without a separate hard-coded schema.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoolingBounds {
    /// Accepted range for `target_temp_c` in Celsius.
    pub target_temp_c: RangeInclusive<f32>,
    /// Accepted range for `pid.min_fan_pwm` as a duty percent.
    pub min_fan_pwm: RangeInclusive<u8>,
    /// Accepted range for `fixed_fan_pwm` as a duty percent.
    pub fixed_fan_pwm: RangeInclusive<u8>,
}

/// Accepted value ranges for [`CoolingConfig`] fields.
pub const BOUNDS: CoolingBounds = CoolingBounds {
    target_temp_c: 30.0..=85.0,
    min_fan_pwm: 0..=100,
    fixed_fan_pwm: 0..=100,
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
