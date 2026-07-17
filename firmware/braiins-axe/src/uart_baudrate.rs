//! `serial_baudrate::BaudRate` wrapper for the embassy-rp `BufferedUart`.
//!
//! `ubos_mining::serial::Serial::run` drives one full-duplex instance, which
//! this branch hands the unsplit `BufferedUart` directly. `BufferedUart`
//! already implements `Read` + `Write`, and embassy-rp 0.10 exposes runtime
//! baud switching via `BufferedUart::set_baudrate(&mut self)` (its internal
//! `lcr_modify` does the PL011 disable/15-baud-delay/restore dance). `MinerUart`
//! is therefore a thin newtype that exists only to satisfy the orphan rule for
//! `BaudRate`.

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
}
