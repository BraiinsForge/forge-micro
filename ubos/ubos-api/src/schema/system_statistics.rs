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

//! Response shape for `GET /api/system/statistics`.
//!
//! The spec calls for a `{currentTimestamp, labels[], statistics[][]}`
//! matrix. The structure is published with a fixed label set and an
//! empty rows slice. A server-side ring buffer can be wired later
//! without a schema change.

use serde::Serialize;

/// Column labels for the statistics matrix.
///
/// The order here is the column order clients must use when
/// interpreting rows in `SystemStatistics::statistics`.
pub const LABELS: &[&str] = &["hashrate", "temperature", "fan_rpm", "fan_pwm", "power_mw"];

/// Statistics time-series snapshot for `GET /api/system/statistics`.
///
/// `statistics` is a slice of row slices; each row contains one
/// sample per label in [`LABELS`] order. Until the server-side ring
/// buffer lands, `statistics` is always empty.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
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
