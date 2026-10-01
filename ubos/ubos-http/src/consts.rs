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

//! Tuning for the listener that serves the web UI and the REST API on
//! one port. Below both handler crates, because each reads `PORT`.

use embassy_time::Duration;

use ubos_core::consts::Net;

pub struct Http;
impl Http {
    /// One origin for the SPA and the JSON endpoints, so a browser sends
    /// no preflight and the firmware carries no CORS headers.
    pub const PORT: u16 = 80;
    /// Receive side: a full segment plus headers, and no larger than the
    /// Ethernet chip's own RX buffer, which cannot be drained while a
    /// flash write freezes both cores.
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    /// One request, headers plus a small POST body.
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
    /// Progress timeout. Resets on progress, so an OTA upload survives
    /// its flash-write freezes.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
    /// Bound on the wait for headers. Short, so an idle kept-alive
    /// browser connection frees its pool slot.
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
}
