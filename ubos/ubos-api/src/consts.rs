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

//! Tuning for the REST API listener on port 8080.

use embassy_time::Duration;

use ubos_core::consts::Net;

/// REST API listener settings (port 8080).
///
/// Buffer sizes are sized independently of the web-UI shell in
/// `ubos-webui`; they mirror it today but tune separately if REST
/// payloads diverge from the SPA's needs.
pub struct Api;
impl Api {
    pub const PORT: u16 = 8080;
    // Kept at or below the W6100's 4 KiB on-chip RX buffer on purpose. Each
    // 4 KiB DFU sector write during an OTA upload pauses both cores, and the
    // W6100 cannot be drained while frozen; a window larger than the chip
    // buffer lets more data arrive than the chip can hold, so frames are
    // dropped and TCP backs off. A 2 KiB window keeps in-flight data under the
    // chip buffer so a single flash freeze cannot overflow it.
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
    /// Per-socket inactivity timeout for the REST listener. Held at the base
    /// `Net::SOCKET_TIMEOUT` because an OTA upload streams here and each
    /// flash-write freeze stalls the socket; the timeout resets on progress,
    /// so a slow link survives as long as bytes keep arriving. The listener
    /// closes after every response, so keepalive reclaim does not apply.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
}
