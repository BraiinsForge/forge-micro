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

//! Cooling loop configuration, from the live schema version.

use core::ops::RangeInclusive;

use crate::thermal::TemperatureDescriptor;
use ubos_core::units::Temperature;

pub use crate::versions::live::{CoolingConfig, FanMode, PidConfig, PidTarget, Statistic};

impl Statistic {
    /// The `stat` label of this reduction in the metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Mean => "mean",
        }
    }
}

impl FanMode {
    /// Wire name, for log lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed => "fixed",
        }
    }
}

impl Default for PidConfig {
    /// Factory-tuned PID gains that every new config starts with.
    fn default() -> Self {
        Self {
            min_fan_pwm: 20,
            p_gain: -5.0,
            i_gain: -0.15,
            d_gain: -100.0,
            smoother_half_decay_secs: 10,
            input_smoother_half_decay_secs: 0,
        }
    }
}

impl CoolingConfig {
    /// `dangerous` holds the halt thresholds the caller resolved: the
    /// incoming ones or the live ones.
    ///
    /// # Errors
    ///
    /// When the temperature is outside the bounds, the machine does not
    /// read the target's kind, or the temperature is not below that kind's
    /// threshold.
    pub fn validate_target(
        target: PidTarget,
        dangerous: TemperatureDescriptor,
    ) -> Result<(), &'static str> {
        if !BOUNDS.target_temp.contains(&target.temp) {
            return Err("cooling.target.temp must be between 30000 and 85000");
        }
        let Some(threshold) = dangerous.temp(target.source) else {
            return Err(
                "cooling.target.source must be a kind of reading that the machine measures",
            );
        };
        if target.temp >= threshold {
            return Err(
                "cooling.target.temp must be less than the dangerous temperature of its kind",
            );
        }
        Ok(())
    }

    /// Zero is accepted: a stopped fan is a legitimate request, and the
    /// mining halt still protects the board.
    ///
    /// # Errors
    ///
    /// When `pwm` is above 100.
    pub fn validate_fixed_fan_pwm(pwm: u8) -> Result<(), &'static str> {
        if !BOUNDS.fixed_fan_pwm.contains(&pwm) {
            return Err("cooling.fixed_fan_pwm must be between 0 and 100");
        }
        Ok(())
    }

    /// # Errors
    ///
    /// When `pwm` is above 100, which would invert the PID clamp range.
    /// The controller clamps it anyway; rejecting it here keeps a value
    /// the operator sees from being silently altered.
    pub fn validate_min_fan_pwm(pwm: u8) -> Result<(), &'static str> {
        if !BOUNDS.min_fan_pwm.contains(&pwm) {
            return Err("cooling.min_fan_pwm must be between 0 and 100");
        }
        Ok(())
    }
}

/// The fixed duty before the operator picks one, so the UI has a value
/// to show when the mode is first switched.
pub(crate) const DEFAULT_FIXED_FAN_PWM: u8 = 50;

/// Accepted ranges for every numeric cooling field.
///
/// `mode` carries no range: it is an enumeration, and a value outside
/// it is rejected. The cooling validators check against these ranges
/// rather than against literals of their own, so this is the single
/// definition of what the firmware accepts, and the API reports it
/// beside the current config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CoolingBounds {
    /// Accepted range for `target.temp`.
    pub target_temp: RangeInclusive<Temperature>,
    /// Accepted range for `pid.min_fan_pwm` as a duty percent.
    pub min_fan_pwm: RangeInclusive<u8>,
    /// Accepted range for `fixed_fan_pwm` as a duty percent.
    pub fixed_fan_pwm: RangeInclusive<u8>,
}

pub const BOUNDS: CoolingBounds = CoolingBounds {
    target_temp: Temperature::from_celsius(30)..=Temperature::from_celsius(85),
    min_fan_pwm: 0..=100,
    fixed_fan_pwm: 0..=100,
};

/// Static bounds metadata for the cooling config section.
///
/// Carries the accepted ranges of the cooling fields so a client can
/// take its input limits from the same source the firmware validates
/// against.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CoolingInfo {
    /// Accepted ranges for the cooling fields.
    pub bounds: &'static CoolingBounds,
}

pub const INFO: CoolingInfo = CoolingInfo { bounds: &BOUNDS };
