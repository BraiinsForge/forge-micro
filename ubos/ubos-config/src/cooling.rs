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
//! Re-exports [`CoolingConfig`], [`FanMode`], [`PidConfig`], [`PidTarget`]
//! and [`Statistic`] from the live version of the config schema, and
//! defines their defaults and validation, [`CoolingBounds`], and the
//! static [`BOUNDS`] constant.

use core::ops::RangeInclusive;

use crate::thermal::{Source, TemperatureDescriptor};
use ubos_asic::{CoolingSource, CoolingStatistic, HashboardDescriptor};
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

impl PidTarget {
    /// The target `board` starts with: the reading, reduction and
    /// setpoint its cooling profile names.
    #[must_use]
    pub const fn default_for(board: &HashboardDescriptor) -> Self {
        let cooling = &board.cooling;
        Self {
            source: match cooling.target_source {
                CoolingSource::Hashboard => Source::Hashboard,
                CoolingSource::Chip => Source::Chip,
            },
            statistic: match cooling.target_statistic {
                CoolingStatistic::Max => Statistic::Max,
                CoolingStatistic::Mean => Statistic::Mean,
            },
            temp: cooling.target_temp,
        }
    }
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

impl Default for PidConfig {
    /// Factory-tuned PID gains for the BMM101 cooling profile.
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

impl Default for CoolingConfig {
    /// Factory default: PID control of the hottest hashboard reading at a
    /// 67 °C setpoint with tuned gains.
    fn default() -> Self {
        Self {
            mode: FanMode::Auto,
            target: PidTarget {
                source: Source::Hashboard,
                statistic: Statistic::Max,
                temp: Temperature::from_celsius(67),
            },
            fixed_fan_pwm: DEFAULT_FIXED_FAN_PWM,
            pid: PidConfig::default(),
        }
    }
}

impl CoolingConfig {
    /// Validate the proposed `target` against [`BOUNDS`] and `dangerous`, the
    /// halt thresholds the caller resolves from the same update or from the
    /// current live config.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the temperature is outside `BOUNDS.target_temp`,
    /// when the machine does not read the target's kind, or when the
    /// temperature is not strictly less than that kind's threshold.
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

/// Accepted value ranges for [`CoolingConfig`] fields.
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

/// Bounds metadata served at `GET /api/system/cooling`.
pub const INFO: CoolingInfo = CoolingInfo { bounds: &BOUNDS };
