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

//! Response shape for `GET /api/system/statistics`: a fixed label set
//! and, until a ring is wired, no rows.

use serde::Serialize;

/// Column labels, in the column order of `SystemStatistics::statistics`.
pub const LABELS: &[&str] = &["hashrate", "temperature", "fan_rpm", "fan_pwm", "power_mw"];

/// Statistics time-series snapshot for `GET /api/system/statistics`.
///
/// `statistics` is an array of rows; each row holds one sample per
/// entry of `labels`, in that order. Until the server-side ring buffer
/// lands, `statistics` is always empty.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemStatistics<'response> {
    /// Unix epoch timestamp in milliseconds at the time of the
    /// response; zero if NTP has not yet synchronized.
    pub current_timestamp: u64,
    /// Column label names; stable across firmware updates.
    pub labels: &'static [&'static str],
    /// Sample rows, one entry per historical snapshot. Empty until the
    /// server-side ring buffer is wired in a later phase.
    pub statistics: &'response [&'response [i64]],
}
