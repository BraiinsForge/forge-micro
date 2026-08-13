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

//! `serial_baudrate::BaudRate` wrapper for the STM32 USART3.
//!
//! `ubos_mining::serial::Serial::run` drives one full-duplex instance built
//! from the DMA ring-buffered RX half and the TX half. embassy-stm32 0.6
//! exposes `UartTx::set_baudrate`, which reconfigures the shared USART3 BRR
//! (so it retunes both directions), disables UE around the write, and selects
//! 8× oversampling automatically using the kernel clock embassy captured at
//! init. `MinerUart` delegates to it; no direct PAC access is needed.

use embassy_stm32::mode::Async;
use embassy_stm32::usart::{RingBufferedUartRx, UartTx};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use serial_baudrate::BaudRate;

/// The BM1362 serial link over USART3: the DMA ring-buffered RX half and the
/// TX half combined, so `ubos_mining::serial::Serial::run` drives one
/// full-duplex instance. Both halves share `embassy_stm32::usart::Error`, and
/// `BaudRate` retunes the line through the TX half's `set_baudrate`.
pub struct MinerUart {
    rx: RingBufferedUartRx<'static>,
    tx: UartTx<'static, Async>,
}

impl MinerUart {
    /// Combine the split USART3 halves into one full-duplex instance.
    #[must_use]
    pub fn new(tx: UartTx<'static, Async>, rx: RingBufferedUartRx<'static>) -> Self {
        Self { rx, tx }
    }
}

impl ErrorType for MinerUart {
    type Error = embassy_stm32::usart::Error;
}

impl Read for MinerUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.rx.read(buffer).await
    }
}

impl Write for MinerUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        // `UartTx::write` inherent method returns `Result<(), Error>`.
        self.tx.write(buffer).await?;
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.tx.flush().await
    }
}

impl BaudRate for MinerUart {
    fn set_baudrate(&mut self, baud: u32) -> Result<(), ErrorKind> {
        // BRR is shared per USART, so retuning the TX half retunes both
        // directions. The only failures are out-of-range rates.
        self.tx
            .set_baudrate(baud)
            .map_err(|_| ErrorKind::Unsupported)
    }
}
