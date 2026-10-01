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

//! Runtime configuration for `BmmPsu`.

use embassy_time::Duration;
use ubos_core::units::{Current, Voltage};

use crate::tps6287x::{SoftStartRampTime, VoltageRampSpeed, VoltageRange};

#[derive(Clone, Copy, Debug)]
pub struct BmmPsuDescriptor {
    pub dcdc_address: u8,
    pub dcdc_initial: Voltage,
    pub dcdc_voltage_range: VoltageRange,
    pub dcdc_ramp_speed: VoltageRampSpeed,
    pub dcdc_soft_start: SoftStartRampTime,
    pub heartbeat_address: u8,
    pub heartbeat_interval: Duration,
    pub ina_input_address: u8,
    pub ina_output_address: u8,
    pub ina_input_max_current: Current,
    pub ina_output_max_current: Current,
    pub ina_input_shunt_microohm: u32,
    pub ina_output_shunt_microohm: u32,
}

impl BmmPsuDescriptor {
    /// Defaults shared verbatim between the BMM100 and BMM101 boards.
    pub const DEFAULT: Self = Self {
        dcdc_address: 0x40,
        dcdc_initial: Voltage::from_mv(750),
        dcdc_voltage_range: VoltageRange::VRange2,
        dcdc_ramp_speed: VoltageRampSpeed::VRamp1_25,
        dcdc_soft_start: SoftStartRampTime::SSTime1,
        heartbeat_address: 0x08,
        heartbeat_interval: Duration::from_secs(5),
        ina_input_address: 0x46,
        ina_output_address: 0x43,
        ina_input_max_current: Current::from_amps(6),
        ina_output_max_current: Current::from_amps(30),
        ina_input_shunt_microohm: 6_000,
        ina_output_shunt_microohm: 1_960,
    };
}
