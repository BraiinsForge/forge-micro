//! `serial_baudrate::BaudRate` wrappers for the ESP32-S3 `UART0` halves.
//!
//! `mmm::serial::SERIAL.run` takes split UART halves and requires each to
//! implement `serial_baudrate::BaudRate`. The split `UartRx` / `UartTx`
//! halves expose `apply_config` but that method only resets the FIFO; it
//! does not touch the clock divider. To change the baud rate without the
//! unsplit `Uart` handle we write the `CLKDIV` register directly via the
//! static `UART0::regs()` accessor.
//!
//! ESP32-S3 Technical Reference Manual §26.3.2: the UART baud-rate generator
//! is driven by `UART_SCLK` (`APB_CLK` = 80 MHz by default). The divider is a
//! 12-bit integer plus a 4-bit fractional field; the hardware computes:
//!
//!   `baud = UART_SCLK_CLK / ((CLKDIV << 4 | FRAG) / 16)`
//!
//! For the integer-only case (`FRAG` = 0):
//!
//!   `CLKDIV = (APB_CLK << 4) / (baud * clk_div)`
//!
//! where `clk_div` is 1 for `APB_CLK` < 31.25 MHz × baud. For the baud rates
//! used here (115 200 – 3 000 000), `APB_CLK` = 80 MHz, so `clk_div` = 1 and
//! `CLKDIV = (80_000_000 << 4) / baud` (with integer truncation).
//!
//! The `mmm` serial actor drains for one second on each side of a baud-rate
//! change, so no in-flight character guard is needed before writing `CLKDIV`.

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use esp_hal::peripherals::UART0;
use esp_hal::uart::{UartRx, UartTx};
use serial_baudrate::BaudRate;

/// `APB_CLK` frequency in Hz. Default clock tree leaves `APB` at 80 MHz.
const APB_CLOCK_HZ: u32 = 80_000_000;

/// Receiving half of `UART0` with `BaudRate` support.
pub struct MmmUartRx {
    inner: UartRx<'static, esp_hal::Async>,
}

/// Transmitting half of `UART0` with `BaudRate` support.
pub struct MmmUartTx {
    inner: UartTx<'static, esp_hal::Async>,
}

/// Wrap the split `UART0` halves. The `UART0` PAC constant is captured
/// implicitly; both sides write the same `CLKDIV` register.
#[must_use]
pub fn split_uart0(
    rx: UartRx<'static, esp_hal::Async>,
    tx: UartTx<'static, esp_hal::Async>,
) -> (MmmUartRx, MmmUartTx) {
    (MmmUartRx { inner: rx }, MmmUartTx { inner: tx })
}

impl ErrorType for MmmUartRx {
    type Error = esp_hal::uart::RxError;
}

impl Read for MmmUartRx {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Read::read(&mut self.inner, buffer).await
    }
}

impl ErrorType for MmmUartTx {
    type Error = esp_hal::uart::TxError;
}

impl Write for MmmUartTx {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Write::write(&mut self.inner, buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io_async::Write::flush(&mut self.inner).await
    }
}

impl BaudRate for MmmUartTx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_uart0_clkdiv(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_uart0_clkdiv()
    }
}

impl BaudRate for MmmUartRx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_uart0_clkdiv(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_uart0_clkdiv()
    }
}

/// Write a new baud rate divisor to `UART0 CLKDIV`.
///
/// ESP32-S3 TRM §26.3.2: `CLKDIV = (APB_CLK << 4) / baud` (with `clk_div = 1`,
/// `FRAG = 0`). The `mmm` serial actor drains for one second on either side of
/// a baud-rate change, so no in-flight character guard is needed here.
/// `UART0::regs()` is a static PAC accessor; the mmm serial actor's internal
/// mutex ensures only one task calls `set_baudrate` at a time.
fn write_uart0_clkdiv(baud: u32) -> Result<(), ErrorKind> {
    if baud == 0 {
        return Err(ErrorKind::InvalidInput);
    }
    // (APB_CLK << 4) / baud gives the combined integer+frag field.
    let divider = (u64::from(APB_CLOCK_HZ) << 4) / u64::from(baud);
    let divider_integer = divider >> 4;
    let divider_frag = (divider & 0xf) as u8;
    if divider_integer == 0 || divider_integer > 0xFFF {
        return Err(ErrorKind::InvalidInput);
    }
    // CLKDIV is a 12-bit field; the range check above guarantees the value fits.
    #[allow(clippy::cast_possible_truncation)]
    let divider_integer = divider_integer as u16;
    // Safety: UART0::regs() returns a stable static register-block pointer.
    // The mmm serial actor serialises set_baudrate calls so no concurrent
    // write races are possible.
    let regs = UART0::regs();
    regs.clkdiv()
        .write(|w| unsafe { w.clkdiv().bits(divider_integer).frag().bits(divider_frag) });
    Ok(())
}

/// Read back the baud rate from `UART0 CLKDIV`.
fn read_uart0_clkdiv() -> Result<u32, ErrorKind> {
    let regs = UART0::regs();
    let reg = regs.clkdiv().read();
    let integer = u64::from(reg.clkdiv().bits());
    let frag = u64::from(reg.frag().bits());
    let combined = (integer << 4) | frag;
    if combined == 0 {
        return Err(ErrorKind::InvalidData);
    }
    // Inverse of the write formula; rounded.
    let baud = (u64::from(APB_CLOCK_HZ) << 4) / combined;
    u32::try_from(baud).map_err(|_| ErrorKind::InvalidData)
}
