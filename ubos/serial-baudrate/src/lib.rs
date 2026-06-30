//! Trait for runtime adjustment of a serial line's baud rate.
//!
//! `embedded-io-async` covers the byte-stream half of a serial port; this
//! crate adds the orthogonal capability of reading and changing the line
//! rate. It's split out so a `no_std` consumer (the BM1362 driver) and a
//! Linux-side implementation can share the trait without either pulling
//! the other in transitively.
//!
//! Errors collapse to `embedded_io::ErrorKind`. Linux backends are
//! expected to log the underlying `io::Error` and map unknown rates to
//! `Unsupported`, anything else to `Other`.

#![no_std]

use embedded_io::ErrorKind;

/// Set the line rate of a serial port. Implementations are expected to be
/// synchronous: changing the baud rate is a tty `tcsetattr`-shaped operation,
/// not an async I/O call. The mining serial actor owns the port exclusively,
/// so the method takes `&mut self` and can delegate straight to the HAL.
pub trait BaudRate {
    /// Apply `baud` to the line. Effective immediately on success.
    ///
    /// # Errors
    ///
    /// `Unsupported` if the rate is not in the backend's table; `Other`
    /// for backend-specific failures (the backend should log the real
    /// error before mapping).
    fn set_baudrate(&mut self, baud: u32) -> Result<(), ErrorKind>;
}
