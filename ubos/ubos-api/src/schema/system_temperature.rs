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

/// The temperatures the thermal loop filters over its window, the
/// ambient reading and the kind the cooling PID follows, returned by
/// `GET /api/system/temperature`. Each temperature is in whole degrees
/// Celsius, and one with no reading inside the window is `null`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemTemperature {
    /// The hottest chip reading of the window, or `null` when no chip
    /// reading is inside it.
    pub chip_max: Option<i32>,
    /// The mean chip reading, as `chipMax`.
    pub chip_mean: Option<i32>,
    /// The hottest hashboard reading of the window, or `null` when no
    /// hashboard reading is inside it.
    pub hashboard_max: Option<i32>,
    /// The mean hashboard reading, as `hashboardMax`.
    pub hashboard_mean: Option<i32>,
    /// The ambient temperature around the board, or `null` where the
    /// board measures none or no ambient reading is inside the window.
    pub ambient: Option<i32>,
    /// The kind of reading the cooling PID follows, or `null` under a
    /// fixed duty and while a failsafe holds the fan at full.
    pub pid_source: Option<Source>,
}
