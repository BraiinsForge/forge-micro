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

//! The cooling config of version 1.

use ubos_core::units::Temperature;

use super::thermal::Source;

/// Which reduction of one kind's readings the cooling PID follows.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Statistic {
    /// The hottest reading.
    Max,
    /// The mean of the readings.
    Mean,
}

/// What the cooling PID holds: one statistic of one kind of reading, at
/// one temperature. That kind is the only reading the PID follows.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PidTarget {
    pub source: Source,
    pub statistic: Statistic,
    pub temp: Temperature,
}

/// How the cooling loop decides the fan duty.
///
/// [`FanMode::Auto`] runs the PID against [`CoolingConfig::target`];
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
    pub p_gain: f64,
    /// Integral gain.
    pub i_gain: f64,
    /// Derivative gain.
    pub d_gain: f64,
    /// Half-decay time for the derivative smoother in seconds.
    pub smoother_half_decay_secs: u32,
    /// Half-decay time of the smoother on the temperature the PID reads,
    /// counted in thermal samples, which are seconds at the default
    /// one-second sample interval. Zero passes the reading through.
    pub input_smoother_half_decay_secs: u32,
}

/// Tunable parameters for the cooling loop.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoolingConfig {
    /// Whether the loop runs the PID or holds a fixed duty.
    pub mode: FanMode,
    /// What the PID holds, and the only reading it follows: a sample that
    /// lacks the target's kind gives the PID nothing to follow. Ignored in
    /// [`FanMode::Fixed`].
    pub target: PidTarget,
    /// Duty cycle held in [`FanMode::Fixed`], 0-100. Not floored by
    /// [`PidConfig::min_fan_pwm`]: the floor is a bound on PID output, and a
    /// fixed duty is what the operator asked for. A board whose hardware
    /// needs a minimum duty clamps it in its own `FanOutput`.
    pub fixed_fan_pwm: u8,
    /// PID controller gains.
    pub pid: PidConfig,
}
