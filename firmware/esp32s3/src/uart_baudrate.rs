//! `serial_baudrate::BaudRate` wrappers for the ESP32-S3 `UART0` halves.
//!
//! `ubos_mining::serial::SERIAL.run` takes split UART halves and requires each to
//! implement `serial_baudrate::BaudRate`. The split `UartRx` / `UartTx`
//! halves expose `apply_config` but that method only resets the FIFO; it
//! does not touch the clock divider. To change the baud rate without the
//! unsplit `Uart` handle we write the `CLKDIV` register directly via the
//! static `UART0::regs()` accessor.
//!
//! ESP32-S3 Technical Reference Manual §26.3.2: the UART baud-rate generator
//! is driven by `UART_SCLK`. esp-hal selects the 40 MHz XTAL as that source by
//! default (`CLK_CONF` `sclk_sel` = 3, SCLK pre-divider 1), not the 80 MHz APB.
//! The divider is a 12-bit integer plus a 4-bit fractional field; the hardware
//! computes:
//!
//!   `baud = UART_SCLK / ((CLKDIV << 4 | FRAG) / 16)`
//!
//! For the integer-only case (`FRAG` = 0):
//!
//!   `CLKDIV = (UART_SCLK << 4) / (baud * clk_div)`
//!
//! where `clk_div` is the SCLK pre-divider, 1 across the baud rates used here
//! (115 200 – 3 000 000), so `CLKDIV = (40_000_000 << 4) / baud` (integer
//! truncation). We leave `sclk_sel` and the pre-divider as esp-hal programmed
//! them in `Uart::new` and only rewrite `CLKDIV`; matching the 40 MHz source
//! is what makes that single-register rewrite reproduce esp-hal's own value.
//!
//! The `ubos-mining` serial actor drains for one second on each side of a
//! baud-rate change, so no in-flight character guard is needed before writing
//! `CLKDIV`.

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use esp_hal::peripherals::UART0;
use esp_hal::uart::{UartRx, UartTx};
use serial_baudrate::BaudRate;

/// UART0 source-clock (`UART_SCLK`) frequency in Hz. esp-hal selects the
/// 40 MHz XTAL as the UART function clock by default (`CLK_CONF` `sclk_sel` = 3,
/// pre-divider 1), not the 80 MHz APB. Verified against the registers esp-hal
/// programs for 115200: clkdiv = 5555 = (40 MHz << 4) / 115200. Using 80 MHz
/// here doubled every divider and halved the real baud (115200 -> 57600), so
/// the chips saw undecodable frames and never answered.
const UART_SCLK_HZ: u32 = 40_000_000;

/// Receiving half of `UART0` with `BaudRate` support.
pub struct MinerUartRx {
    inner: UartRx<'static, esp_hal::Async>,
}

/// Transmitting half of `UART0` with `BaudRate` support.
pub struct MinerUartTx {
    inner: UartTx<'static, esp_hal::Async>,
}

/// Wrap the split `UART0` halves. The `UART0` PAC constant is captured
/// implicitly; both sides write the same `CLKDIV` register.
#[must_use]
pub fn split_uart0(
    rx: UartRx<'static, esp_hal::Async>,
    tx: UartTx<'static, esp_hal::Async>,
) -> (MinerUartRx, MinerUartTx) {
    (MinerUartRx { inner: rx }, MinerUartTx { inner: tx })
}

impl ErrorType for MinerUartRx {
    type Error = esp_hal::uart::RxError;
}

impl Read for MinerUartRx {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Read::read(&mut self.inner, buffer).await
    }
}

impl ErrorType for MinerUartTx {
    type Error = esp_hal::uart::TxError;
}

impl Write for MinerUartTx {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Write::write(&mut self.inner, buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io_async::Write::flush(&mut self.inner).await
    }
}

impl BaudRate for MinerUartTx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_uart0_clkdiv(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_uart0_clkdiv()
    }
}

impl BaudRate for MinerUartRx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_uart0_clkdiv(baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_uart0_clkdiv()
    }
}

/// Write a new baud rate divisor to `UART0 CLKDIV`.
///
/// ESP32-S3 TRM §26.3.2: `CLKDIV = (UART_SCLK << 4) / baud` (with the SCLK
/// pre-divider 1). The `ubos-mining` serial actor drains for one second on either
/// side of a baud-rate change, so no in-flight character guard is needed here.
/// `UART0::regs()` is a static PAC accessor; the ubos-mining serial actor's internal
/// mutex ensures only one task calls `set_baudrate` at a time.
fn write_uart0_clkdiv(baud: u32) -> Result<(), ErrorKind> {
    if baud == 0 {
        return Err(ErrorKind::InvalidInput);
    }
    // (UART_SCLK << 4) / baud gives the combined integer+frag field.
    let divider = (u64::from(UART_SCLK_HZ) << 4) / u64::from(baud);
    let divider_integer = divider >> 4;
    let divider_frag = (divider & 0xf) as u8;
    if divider_integer == 0 || divider_integer > 0xFFF {
        return Err(ErrorKind::InvalidInput);
    }
    // CLKDIV is a 12-bit field; the range check above guarantees the value fits.
    #[allow(clippy::cast_possible_truncation)]
    let divider_integer = divider_integer as u16;
    // Safety: UART0::regs() returns a stable static register-block pointer.
    // The ubos-mining serial actor serialises set_baudrate calls so no concurrent
    // write races are possible.
    let regs = UART0::regs();
    regs.clkdiv()
        .write(|w| unsafe { w.clkdiv().bits(divider_integer).frag().bits(divider_frag) });
    // The ESP32-S3 latches CLKDIV (and other UART config) into the core only
    // when the `reg_update` bit in the UART_ID register is pulsed; esp-hal does
    // this in its `sync_regs`. Without it the divisor write never takes effect,
    // so the first real baud change (115200 -> 3.125 Mbaud) leaves the line
    // mis-clocked and the serial worker hangs (super-WDT reset). The boot baud
    // worked only because `Uart::new` already synced 115200. The bit
    // self-clears within a few core cycles, matching esp-hal's spin.
    regs.id().modify(|_, w| w.reg_update().set_bit());
    while regs.id().read().reg_update().bit_is_set() {}
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
    let baud = (u64::from(UART_SCLK_HZ) << 4) / combined;
    u32::try_from(baud).map_err(|_| ErrorKind::InvalidData)
}
