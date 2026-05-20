//! `serial_baudrate::BaudRate` wrappers for the STM32F407 USART3 halves.
//!
//! embassy-stm32 0.6.0 exposes `Uart::set_baudrate` on the unsplit peripheral
//! with `&self`, but the split `UartTx`/`RingBufferedUartRx` halves do not
//! expose baud-rate control. `ubos_miner::serial::SERIAL.run` takes the two halves
//! and requires each to implement `serial_baudrate::BaudRate`. This module
//! wraps each half with a direct PAC write to USART3's BRR register so either
//! side can retune the line.
//!
//! The STM32F407 USART3 is clocked from APB1. At the default clock tree
//! (HSE 8 MHz → PLL 168 MHz sys, APB1 /4) the APB1 clock is 42 MHz.
//! BRR = (`APB1_CLK` / baud) for 16× oversampling (OVER8 = 0).
//! RM0090 §30.5.4: disable UE before writing BRR, then re-enable.

use embassy_stm32::mode::Async;
use embassy_stm32::pac;
use embassy_stm32::usart::{RingBufferedUartRx, UartTx};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use serial_baudrate::BaudRate;

/// APB1 clock frequency used when computing the BRR divisor.
/// Must match the `apb1_pre = DIV4` entry in `config::stm32_config()`.
const APB1_CLOCK_HZ: u32 = 42_000_000;

/// Receiving half of USART3 with `BaudRate` support.
pub struct MinerUartRx {
    inner: RingBufferedUartRx<'static>,
}

/// Transmitting half of USART3 with `BaudRate` support.
pub struct MinerUartTx {
    inner: UartTx<'static, Async>,
}

/// Wrap the split USART3 halves. Hard-codes `pac::USART3` as the register
/// block so either side can retune the line without a separate PAC argument.
#[must_use]
pub fn split_usart3(
    tx: UartTx<'static, Async>,
    rx: RingBufferedUartRx<'static>,
) -> (MinerUartTx, MinerUartRx) {
    (MinerUartTx { inner: tx }, MinerUartRx { inner: rx })
}

impl ErrorType for MinerUartRx {
    type Error = embassy_stm32::usart::Error;
}

impl Read for MinerUartRx {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buffer).await
    }
}

impl ErrorType for MinerUartTx {
    type Error = embassy_stm32::usart::Error;
}

impl Write for MinerUartTx {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        // `UartTx::write` inherent method returns `Result<(), Error>`.
        self.inner.write(buffer).await?;
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

impl BaudRate for MinerUartTx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_usart3_brr(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_usart3_brr()
    }
}

impl BaudRate for MinerUartRx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_usart3_brr(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_usart3_brr()
    }
}

/// Write a new baud rate divisor to USART3 BRR.
///
/// RM0090 §30.5.4 requires UE = 0 before changing BRR. This function
/// clears UE, writes BRR, then restores UE. The ubos-miner backend drains
/// for one second on either side of a baud-rate change, so no
/// in-flight character guard is needed here.
fn write_usart3_brr(baud: u32) -> Result<(), ErrorKind> {
    if baud == 0 {
        return Err(ErrorKind::InvalidInput);
    }
    // 16× oversampling (OVER8 = 0): BRR = APB1_CLK / baud.
    let brr = APB1_CLOCK_HZ / baud;
    if !(16..=0xFFFF).contains(&brr) {
        return Err(ErrorKind::InvalidInput);
    }
    // USART3 is a valid peripheral constant; the ubos-miner serial actor's
    // caller mutex ensures only one task drives set_baudrate at a time.
    let regs = pac::USART3;
    let cr1 = regs.cr1().read();
    if cr1.ue() {
        regs.cr1().modify(|w| {
            w.set_ue(false);
        });
    }
    // BRR field is 16 bits; brr fits because we checked above.
    #[allow(clippy::cast_possible_truncation)]
    regs.brr().modify(|w| {
        w.set_brr(brr as u16);
    });
    if cr1.ue() {
        regs.cr1().modify(|w| {
            w.set_ue(true);
        });
    }
    Ok(())
}

/// Read back the baud rate from USART3 BRR.
fn read_usart3_brr() -> Result<u32, ErrorKind> {
    let brr = u32::from(pac::USART3.brr().read().brr());
    if brr == 0 {
        return Err(ErrorKind::InvalidData);
    }
    Ok(APB1_CLOCK_HZ / brr)
}
