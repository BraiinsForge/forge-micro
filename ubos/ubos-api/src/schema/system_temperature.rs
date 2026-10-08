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

//! Response schema for `GET /api/system/temperature`.

use serde::Serialize;
use ubos_config::Source;

/// The temperatures of the last thermal sample, the ambient reading and
/// the kind the cooling PID follows, returned by
/// `GET /api/system/temperature`. Each temperature is in whole degrees
/// Celsius, and one the last sample did not read is `null`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemTemperature {
    /// The hottest chip reading, or `null` when the sample read no chip.
    pub chip_max: Option<i32>,
    /// The mean chip reading, as `chip_max`.
    pub chip_mean: Option<i32>,
    /// The hottest hashboard reading, or `null` when the sample read no
    /// hashboard sensor.
    pub hashboard_max: Option<i32>,
    /// The mean hashboard reading, as `hashboard_max`.
    pub hashboard_mean: Option<i32>,
    /// The ambient temperature around the board, or `null` where the
    /// board measures none or the read failed.
    pub ambient: Option<i32>,
    /// The kind of reading the cooling PID follows, or `null` under a
    /// fixed duty and while a failsafe holds the fan at full.
    pub pid_source: Option<Source>,
}
