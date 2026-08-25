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

//! Tuning for the listener that serves the web UI and the REST API.
//!
//! The two surfaces answer on one port behind [`crate::PrefixRouter`], so
//! one set of constants describes both. They live here, below both
//! handler crates, because `ubos-api` and `ubos-webui` each read `PORT`
//! at item position for their mDNS entry.

use embassy_time::Duration;

use ubos_core::consts::Net;

/// Settings for the listener that serves the web UI at `/` and the REST
/// API under `/api/`.
pub struct Http;
impl Http {
    /// The port both surfaces answer on. One origin serves the SPA and
    /// the JSON endpoints, so a browser sends no preflight and the
    /// firmware carries no CORS headers.
    pub const PORT: u16 = 80;
    /// Receive side of the connection socket. 2 KiB is ~1.4x the
    /// Ethernet MSS of 1460, so a full request segment plus headers fits
    /// without inflating .bss on the 128 KiB stm32f407. It is also at or
    /// below the W6100's 4 KiB on-chip RX buffer on purpose: each 4 KiB
    /// DFU sector write during an OTA upload pauses both cores, and the
    /// W6100 cannot be drained while frozen, so a window larger than the
    /// chip buffer lets more data arrive than the chip can hold and
    /// frames are dropped.
    pub const RX_BUFFER_SIZE: usize = 2048;
    /// Send side, held level with the receive side.
    pub const TX_BUFFER_SIZE: usize = 2048;
    /// Buffer for reading one incoming request. Sized to hold a small
    /// POST body alongside headers so the config form fits in a single
    /// read; the route line is parsed from the start of the same buffer.
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
    /// Per-socket progress timeout. Held at the base `Net::SOCKET_TIMEOUT`
    /// because an OTA upload streams here and each flash-write freeze
    /// stalls the socket; the timeout resets on progress, so a slow link
    /// survives as long as bytes keep arriving.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
    /// Bound on the wait for the request headers. Short on purpose: the
    /// listener keeps a connection alive between requests, so a browser
    /// that has finished loading the page must free its worker rather
    /// than pin one of the few pool slots for the full progress bound.
    /// It bounds the first request on a new connection as well, so a
    /// machine client that opens a socket and then waits loses it.
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
}
