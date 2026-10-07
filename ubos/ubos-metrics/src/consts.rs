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
    /// Receive side of the scrape socket. 512 bytes covers the ~390-byte
    /// Prometheus 3.x request. Trimmed from 1024 so the enlarged
    /// `REQUEST_BUFFER_SIZE` below nets a RAM reduction overall: the rp2040
    /// runs the full miner firmware plus this server in 256 KiB and had
    /// only ~240 bytes of free SRAM, so every metrics static is sized
    /// against that ceiling.
    pub const RX_BUFFER_SIZE: usize = 512;
    /// Send side. Kept at 1024 when the receive side was trimmed, because
    /// the exposition response is the large direction on this socket.
    pub const TX_BUFFER_SIZE: usize = 1024;
    /// Buffer for parsing one scraper request. `edge-http` needs the whole
    /// request line plus header block to fit here at once, not just the
    /// `GET /metrics` line. A Prometheus 3.x scrape negotiates `OpenMetrics`
    /// and UTF-8 escaping in a ~240-byte Accept header, ~390 bytes of
    /// request in total; the former 256-byte buffer truncated that, header
    /// parsing failed, and the connection was dropped with no response,
    /// which the scraper saw as an EOF. 512 holds that request with
    /// headroom while staying within the rp2040 SRAM ceiling noted on
    /// `RX_BUFFER_SIZE`; 1024 overflowed that board by ~530 bytes.
    pub const REQUEST_BUFFER_SIZE: usize = 512;
    /// Per-socket progress timeout for the scrape listener. The base
    /// value; a scrape completes in one request and the listener closes.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
    /// Bound on the wait for the request headers. Held at the same base
    /// value the progress timeout carries, which is what one socket timeout
    /// gave this listener before the two waits were separated. The two
    /// listener macros read one constants type, so this listener declares
    /// the bound even though a scrape never reaches it twice.
    pub const IDLE_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
    pub const CHANNEL_DEPTH: usize = 32;
    pub const MAX_LABELS: usize = 4;
}
