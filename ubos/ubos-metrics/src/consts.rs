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

//! Tuning for the Prometheus metrics exposition listener on port 8081.

use embassy_time::Duration;

use ubos_core::consts::Net;

/// Prometheus metrics exposition server settings.
pub struct Metrics;
impl Metrics {
    pub const PORT: u16 = 8081;
    /// Receive side of the scrape socket; a Prometheus 3.x request is
    /// about 390 bytes. Every static here is sized against the rp2040's
    /// SRAM, the smallest board that runs this server.
    pub const RX_BUFFER_SIZE: usize = 512;
    /// Send side, the large direction on this socket, so it kept its size
    /// when the receive side was trimmed.
    pub const TX_BUFFER_SIZE: usize = 1024;
    /// Buffer for one scraper request, which `edge-http` must hold whole.
    /// A Prometheus 3.x scrape carries a 240-byte Accept header, about
    /// 390 bytes in all, and a request that does not fit is dropped with
    /// no response. 1024 overflows the rp2040.
    pub const REQUEST_BUFFER_SIZE: usize = 512;
    /// Per-socket progress timeout.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
    /// Bound on the wait for the request headers. Declared because the
    /// listener macros read one constants type; a scrape never reaches it.
    pub const IDLE_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
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
