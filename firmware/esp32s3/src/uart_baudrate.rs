//! `serial_baudrate::BaudRate` wrapper for the ESP32-S3 `UART0`.
//!
//! `ubos_mining::serial::Serial::run` drives one full-duplex instance. esp-hal
//! exposes runtime baud control only on the unsplit `Uart` (the split
//! `UartRx`/`UartTx::apply_config` reset their FIFOs but never touch the clock
//! divider), so `MinerUart` holds the unsplit `Uart<'static, Async>` and
//! retunes via `Uart::apply_config`. That path reprograms `CLKDIV` from the
//! configured source clock and pulses `reg_update` itself, replacing the
//! hand-written register sequence this module used to carry.
//!
//! `Uart`'s `embedded_io` error type is `esp_hal::uart::IoError`, which already
//! unifies the RX and TX error halves, so no local error enum is needed.

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use esp_hal::uart::{Config as UartConfig, IoError, Uart};
use serial_baudrate::BaudRate;

/// The BM1362 serial link over `UART0`. `ubos_mining::serial::Serial::run`
/// drives a single full-duplex instance, so `MinerUart` holds the unsplit
/// `Uart` and delegates `Read`/`Write`/`set_baudrate` straight to it.
pub struct MinerUart {
    inner: Uart<'static, esp_hal::Async>,
}

impl MinerUart {
    /// Wrap the unsplit `UART0` for use as the BM1362 serial link.
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
