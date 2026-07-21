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

//! `serial_baudrate::BaudRate` wrapper for the embassy-rp `BufferedUart`.
//!
//! `ubos_mining::serial::Serial::run` drives one full-duplex instance, which
//! this branch hands the unsplit `BufferedUart` directly. `BufferedUart`
//! already implements `Read` + `Write`, and embassy-rp 0.10 exposes runtime
//! baud switching via `BufferedUart::set_baudrate(&mut self)` (its internal
//! `lcr_modify` does the PL011 disable/15-baud-delay/restore dance). `MinerUart`
//! is therefore a thin newtype that exists only to satisfy the orphan rule for
//! `BaudRate`.

use embassy_rp::interrupt::{self, InterruptExt};
use embassy_rp::uart::{BufferedUart, Error as UartError};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use serial_baudrate::BaudRate;

/// The BM1362 serial link: the unsplit `BufferedUart`. It is full-duplex and
/// already implements `Read` + `Write`, so the ubos-mining actor drives it as
/// one instance rather than split halves.
pub struct MinerUart {
    inner: BufferedUart,
}

impl MinerUart {
    /// Wrap a `BufferedUart` for use as the BM1362 serial link.
    #[must_use]
    pub fn new(uart: BufferedUart) -> Self {
        Self { inner: uart }
    }
}

impl ErrorType for MinerUart {
    type Error = UartError;
}

impl Read for MinerUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buffer).await
    }
}

impl Write for MinerUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

impl BaudRate for MinerUart {
    fn set_baudrate(&mut self, baud: u32) -> Result<(), ErrorKind> {
        // The ubos-mining actor rejects a zero rate before it reaches here and
        // drains 1 s on either side of the change.
        self.inner.set_baudrate(baud);
        Ok(())
    }

    fn set_rx_irq_enabled(&mut self, enabled: bool) {
        // Masked on core 1 across a config/OTA flash erase so a chip-UART RX
        // interrupt cannot wake the core out of the flash-suspend pause and
        // fault it. `enable` is re-arming an interrupt that was set up at
        // bring-up and only briefly masked here.
        if enabled {
            unsafe { interrupt::UART1_IRQ.enable() };
        } else {
            interrupt::UART1_IRQ.disable();
        }
    }
}
