//! `serial_baudrate::BaudRate` wrapper for the STM32F439 USART3.
//!
//! embassy-stm32 0.6.0 exposes `Uart::set_baudrate` on the unsplit peripheral
//! with `&self`, but the split `UartTx`/`RingBufferedUartRx` halves do not
//! expose baud-rate control. `ubos_mining::serial::SERIAL.run` drives one
//! full-duplex instance implementing `serial_baudrate::BaudRate`. This module
//! combines the halves into a single `MinerUart` and retunes the line with a
//! direct PAC write to USART3's BRR register.
//!
//! The STM32F439 USART3 is clocked from APB1. At the default clock tree
//! (HSE 8 MHz bypass → PLL 180 MHz sys, APB1 /4) the APB1 clock is 45 MHz.
//! Uses 8× oversampling (OVER8 = 1) so the BM1362 fast baud rate of
//! 3 Mbaud is reachable: at 16× the BRR mantissa would need to be < 1
//! (APB1 / 16 / 3 000 000 < 1), which the silicon rejects.
//! At 8× the encoding is BRR[15:4] = mantissa, BRR[2:0] = frac3,
//! BRR[3] reserved, with `(mantissa * 8) + frac3 = APB1 / baud`.
//! RM0090 §30.5.4: disable UE before writing BRR or OVER8, then re-enable.

use embassy_stm32::mode::Async;
use embassy_stm32::pac;
use embassy_stm32::usart::{RingBufferedUartRx, UartTx};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use serial_baudrate::BaudRate;

/// APB1 clock frequency used when computing the BRR divisor.
/// Must match the `apb1_pre = DIV4` entry in `config::stm32_config()`.
/// F439 Nucleo: 180 MHz sysclk / 4 = 45 MHz APB1.
const APB1_CLOCK_HZ: u32 = 45_000_000;

/// The BM1362 serial link over USART3: the DMA ring-buffered RX half and the
/// TX half combined, so `ubos_mining::serial::Serial::run` drives one
/// full-duplex instance. Both halves share `embassy_stm32::usart::Error`, and
/// `BaudRate` writes USART3's BRR directly.
pub struct MinerUart {
    rx: RingBufferedUartRx<'static>,
    tx: UartTx<'static, Async>,
}

impl MinerUart {
    /// Combine the split USART3 halves. The baud path hard-codes `pac::USART3`,
    /// so no PAC argument is needed.
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
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_usart3_brr(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_usart3_brr()
    }
}

/// Write a new baud rate divisor to USART3 BRR and pin OVER8 = 1.
///
/// RM0090 §30.5.4 requires UE = 0 before changing BRR or OVER8. This
/// function clears UE, sets OVER8 = 1, writes BRR encoded for 8×
/// oversampling, then restores UE. The ubos-mining backend drains for
/// one second on either side of a baud-rate change, so no in-flight
/// character guard is needed here.
fn write_usart3_brr(baud: u32) -> Result<(), ErrorKind> {
    if baud == 0 {
        return Err(ErrorKind::InvalidInput);
    }
    // OVER8 = 1: `(mantissa * 8) + frac3 = APB1 / baud`. Mantissa
    // must be >= 1, so the integer divisor must be >= 8. Maximum is
    // 0xFFF mantissa * 8 + 7 = 0x7FFF.
    let usartdiv_x8 = APB1_CLOCK_HZ / baud;
    if !(8..=0x7FFF).contains(&usartdiv_x8) {
        return Err(ErrorKind::InvalidInput);
    }
    // BRR layout under OVER8 = 1: mantissa in [15:4], frac3 in [2:0],
    // bit 3 reserved (kept clear).
    let brr = ((usartdiv_x8 & !0x7) << 1) | (usartdiv_x8 & 0x7);
    // USART3 is a valid peripheral constant; the ubos-mining serial actor's
    // caller mutex ensures only one task drives set_baudrate at a time.
    let regs = pac::USART3;
    let cr1_before = regs.cr1().read();
    regs.cr1().modify(|w| {
        w.set_ue(false);
        w.set_over8(pac::usart::vals::Over8::OVERSAMPLING8);
    });
    // BRR field is 16 bits; brr fits because we checked above.
    #[allow(clippy::cast_possible_truncation)]
    regs.brr().modify(|w| {
        w.set_brr(brr as u16);
    });
    if cr1_before.ue() {
        regs.cr1().modify(|w| {
            w.set_ue(true);
        });
    }
    Ok(())
}

/// Read back the baud rate from USART3 BRR, assuming OVER8 = 1.
fn read_usart3_brr() -> Result<u32, ErrorKind> {
    let brr = u32::from(pac::USART3.brr().read().brr());
    let mantissa = (brr >> 4) & 0xFFF;
    let frac3 = brr & 0x7;
    let usartdiv_x8 = mantissa * 8 + frac3;
    if usartdiv_x8 == 0 {
        return Err(ErrorKind::InvalidData);
    }
    Ok(APB1_CLOCK_HZ / usartdiv_x8)
}
