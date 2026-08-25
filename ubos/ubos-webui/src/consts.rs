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

//! Tuning for the web-UI HTTP listener on port 80.

use embassy_time::Duration;

/// Plain HTTP server settings for the web-UI listener on port 80.
pub struct Http;
impl Http {
    pub const PORT: u16 = 80;
    /// Per-direction TCP socket buffer for the HTTP listener. 2 KiB is
    /// ~1.4× Ethernet MSS (1460), enough to hold a full response segment
    /// plus headers without inflating .bss on the 128 KiB stm32f407.
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    /// Buffer for reading incoming HTTP requests. Sized to hold a small
    /// POST body alongside headers so the config form fits in a single
    /// read; the route line is parsed from the start of the same buffer.
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
    /// Per-socket progress timeout for the web-UI listener. Short on
    /// purpose: every web-UI response is a small flash-backed asset served
    /// in milliseconds, so nothing here needs the full base timeout to make
    /// progress.
    pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(3);
    /// Bound on the wait for the request headers. The listener keeps
    /// connections alive, so a browser that has finished loading the page
    /// must free its worker quickly rather than pin one of the few pool
    /// slots. The bound restarts on each request, so a page's burst of
    /// asset fetches keeps one connection alive throughout.
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
}
