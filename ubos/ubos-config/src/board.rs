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

//! The values a hashboard model hands to a new config.
//!
//! Defines [`HashboardDefaults`]. This crate declares the type and holds
//! no value of it: each hashboard model states its own value beside its
//! hardware, and the firmware passes that value to [`crate::run`].

use ubos_core::units::{Efficiency, FanPwm, Frequency, Voltage};

use crate::cooling::PidTarget;
use crate::thermal::TemperatureDescriptor;

/// The values one hashboard model starts a new config with.
///
/// A config is built from these on a first boot, on a factory reset and
/// when the stored record is lost, so the config schema reads them as plain
/// values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HashboardDefaults {
    /// Clock a fixed tuning starts on.
    pub frequency: Frequency,
    /// Rail voltage a fixed tuning starts on.
    pub voltage: Voltage,
    /// Efficiency a power tuning starts on, so an operator who picks one
    /// has a value that suits the board.
    pub efficiency: Efficiency,
    /// Halt threshold for each kind of reading the machine measures. A
    /// kind the machine reads no sensor for is `None`. The stored
    /// thresholds are replaced with these when a firmware update changes
    /// the kinds.
    pub dangerous: TemperatureDescriptor,
    /// The reading, reduction and setpoint the cooling PID starts on. A
    /// setpoint is compared with the kind of reading it names, so a board
    /// that tracks a hashboard reading sets it below the die temperature
    /// it means.
    pub cooling_target: PidTarget,
    /// Fan duty the cooling PID never goes below.
    pub min_fan_pwm: FanPwm,
    /// Half-decay time of the smoother on the temperature the cooling PID
    /// reads, counted in thermal samples, which are seconds at the default
    /// one-second sample interval. Zero passes the reading through.
    pub input_smoother_half_decay_secs: u32,
}
