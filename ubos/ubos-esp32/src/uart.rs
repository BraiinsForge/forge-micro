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

//! `serial_baudrate::BaudRate` wrapper for the ESP32-S3 `UART0`.
//!
//! esp-hal exposes runtime baud control only on the unsplit `Uart`: the split
//! `UartRx`/`UartTx::apply_config` reset their FIFOs but never touch the clock
//! divider. `MinerUart` therefore holds the unsplit `Uart<'static, Async>` and
//! retunes through `Uart::apply_config`, which reprograms `CLKDIV` from the
//! configured source clock and pulses `reg_update` itself.
//!
//! `Uart`'s `embedded_io` error type is `esp_hal::uart::IoError`, which already
//! unifies the RX and TX error halves, so no local error enum is needed.

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use esp_hal::uart::{Config as UartConfig, IoError, Uart};
use serial_baudrate::BaudRate;

/// The chip serial link over `UART0`, driven as one full-duplex instance.
pub struct MinerUart {
    inner: Uart<'static, esp_hal::Async>,
}

impl MinerUart {
    #[must_use]
    pub fn new(uart: Uart<'static, esp_hal::Async>) -> Self {
        Self { inner: uart }
    }
}

impl ErrorType for MinerUart {
    type Error = IoError;
}

// `Uart`'s inherent methods shadow the async trait methods, so dispatch
// through the traits explicitly.
impl Read for MinerUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Read::read(&mut self.inner, buffer).await
    }
}

impl Write for MinerUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        Write::write(&mut self.inner, buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Write::flush(&mut self.inner).await
    }
}

impl BaudRate for MinerUart {
    fn set_baudrate(&mut self, baud: u32) -> Result<(), ErrorKind> {
        // `Config::default()` is the 8N1 config `Uart::new` was built with, so
        // re-applying it with the new baud reproduces the boot settings on the
        // new rate. The ubos-mining actor drains 1 s on either side.
        self.inner
            .apply_config(&UartConfig::default().with_baudrate(baud))
            .map_err(|_| ErrorKind::Unsupported)
    }
}
