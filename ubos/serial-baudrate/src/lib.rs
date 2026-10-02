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
//! crate adds the orthogonal capability of changing the line rate. It is
//! its own crate so a `no_std` consumer and a Linux-side implementation
//! can share the trait without either pulling the other in transitively.

#![no_std]

use embedded_io::ErrorKind;

/// Set the line rate of a serial port. The methods are synchronous because
/// changing a baud rate is a tty `tcsetattr`-shaped operation, and take
/// `&mut self` because the port's owner holds it exclusively.
pub trait BaudRate {
    /// Apply `baud` to the line. Effective immediately on success.
    ///
    /// # Errors
    ///
    /// `Unsupported` if the rate is not in the backend's table; `Other`
    /// for backend-specific failures, which the backend logs before it
    /// maps them.
    fn set_baudrate(&mut self, baud: u32) -> Result<(), ErrorKind>;

    /// Enable or disable the line's RX interrupt.
    ///
    /// A caller masks it while another core runs a flash erase or program
    /// with XIP suspended: on dual-core RP boards a pending chip-UART RX
    /// interrupt would wake the paused core out of `wfe` and fault it
    /// fetching the pause loop from suspended flash. The default is a no-op,
    /// which single-core and host backends keep.
    fn set_rx_irq_enabled(&mut self, _enabled: bool) {}
}
