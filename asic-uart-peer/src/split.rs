//! Read/Write adapter that fuses a separate UART RX half and TX
//! half into one type implementing both traits.
//!
//! STM32 boards expose `RingBufferedUartRx` and `UartTx` as
//! distinct types; the worker loop needs a single `Read + Write`,
//! so this trivial wrapper merges the halves.

use embedded_io_async::{Error as IoError, Read, Write};

/// Combines a separate RX half and TX half into a single value
/// that implements both [`Read`] and [`Write`].
pub struct SplitUart<R, W> {
    /// The receive half.
    pub rx: R,
    /// The transmit half.
    pub tx: W,
}

impl<R: Read, W: Write> embedded_io_async::ErrorType for SplitUart<R, W> {
    type Error = embedded_io_async::ErrorKind;
}

impl<R: Read, W: Write> Read for SplitUart<R, W> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.rx
            .read(buffer)
            .await
            .map_err(|error| IoError::kind(&error))
    }
}

impl<R: Read, W: Write> Write for SplitUart<R, W> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.tx
            .write(buffer)
            .await
            .map_err(|error| IoError::kind(&error))
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.tx.flush().await.map_err(|error| IoError::kind(&error))
    }
}
