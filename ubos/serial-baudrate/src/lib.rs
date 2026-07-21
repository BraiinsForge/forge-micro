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

//! Trait for runtime adjustment of a serial line's baud rate.
//!
//! `embedded-io-async` covers the byte-stream half of a serial port; this
//! crate adds the orthogonal capability of reading and changing the line
//! rate. It's split out so a `no_std` consumer (the BM1362 driver) and a
//! Linux-side implementation can share the trait without either pulling
//! the other in transitively.
//!
//! Errors collapse to `embedded_io::ErrorKind`. Linux backends are
//! expected to log the underlying `io::Error` and map unknown rates to
//! `Unsupported`, anything else to `Other`.

#![no_std]

use embedded_io::ErrorKind;

/// Set the line rate of a serial port. Implementations are expected to be
/// synchronous: changing the baud rate is a tty `tcsetattr`-shaped operation,
/// not an async I/O call. The mining serial actor owns the port exclusively,
/// so the method takes `&mut self` and can delegate straight to the HAL.
pub trait BaudRate {
    /// Apply `baud` to the line. Effective immediately on success.
    ///
    /// # Errors
    ///
    /// `Unsupported` if the rate is not in the backend's table; `Other`
    /// for backend-specific failures (the backend should log the real
    /// error before mapping).
    fn set_baudrate(&mut self, baud: u32) -> Result<(), ErrorKind>;

    /// Enable or disable the line's RX interrupt.
    ///
    /// The mining serial actor disables it while another core runs a flash
    /// erase/program with XIP suspended: on dual-core RP boards a pending
    /// chip-UART RX interrupt would wake the paused core out of `wfe` and
    /// fault it fetching the pause loop from suspended flash. Default is a
    /// no-op — single-core and host backends are unaffected; only the
    /// dual-core RP backends override it to mask their chip-UART IRQ.
    fn set_rx_irq_enabled(&mut self, _enabled: bool) {}
}
