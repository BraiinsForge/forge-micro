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

//! Tuning for the metrics store: the channel, the labels and the TTL.

use embassy_time::Duration;

/// Metrics store settings.
pub struct Metrics;
impl Metrics {
    pub const CHANNEL_DEPTH: usize = 32;
    pub const MAX_LABELS: usize = 4;
    /// Pushes a series may miss in a row before it expires. Two lost
    /// samples in a row do not remove a series. A publisher with its own
    /// rate multiplies its interval by this to state its TTL.
    pub const TTL_INTERVALS: u32 = 3;
    /// The slowest interval at which a series is pushed, the upper bound
    /// of the thermal sample interval.
    pub const SLOWEST_PUSH_INTERVAL: Duration = Duration::from_secs(60);
    /// The TTL of a measured metric that states none.
    pub const DEFAULT_TTL: Duration =
        Duration::from_secs(Self::SLOWEST_PUSH_INTERVAL.as_secs() * Self::TTL_INTERVALS as u64);
}
