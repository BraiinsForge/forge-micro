//! I2C adapter for Linux `/dev/i2c-N` character devices.
//!
//! Exposes `embedded_hal_async::i2c::I2c` over `I2C_RDWR` ioctls
//! on a synchronous fd; the kernel keeps individual ioctl
//! latencies short enough that an async reactor is not worth its
//! overhead.

#![warn(missing_docs)]
// libc FFI: every `as` cast below is bounded by the kernel ABI.
// Module-scope allow keeps the call sites readable.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::ffi::CString;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;

const I2C_M_RD: u16 = 0x0001;
/// `libc::Ioctl` is `c_ulong` on glibc and `c_int` on musl, so use the
/// type alias rather than a fixed integer width.
const I2C_RDWR: libc::Ioctl = 0x0707;
/// Maximum number of `Operation`s per `transaction()` call. The
/// Linux kernel enforces its own per-driver bound; eight covers
/// every real-world transfer the BMM101 PSU drivers issue.
const MAX_MSGS: usize = 8;

#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}

#[repr(C)]
struct I2cRdwrIoctlData {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}

/// Owned file descriptor that closes on drop.
struct Fd(RawFd);

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        // Best-effort close; ignore errors to keep Drop infallible.
        unsafe { libc::close(self.0) };
    }
}

/// Async I2C handle backed by a Linux `/dev/i2c-N` character device.
pub struct I2c {
    fd: Fd,
}

impl I2c {
    /// Opens an I2C bus character device.
    ///
    /// The fd is opened without `O_NONBLOCK`; `I2C_RDWR` ioctls block
    /// briefly enough that wrapping the fd in an async reactor would
    /// only add overhead.
    ///
    /// # Errors
    /// Returns the underlying `open(2)` error if the path does
    /// not exist, the caller lacks permissions, or the path
    /// contains a NUL byte.
    pub fn open(path: &str) -> io::Result<Self> {
        let cpath = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;

        let raw = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR) };
        if raw == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { fd: Fd(raw) })
    }
}

/// Errors surfaced by [`I2c`].
#[derive(Debug)]
pub enum Error {
    /// Wrapping I/O error from the kernel.
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Io(error) => write!(formatter, "i2c io: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
        }
    }
}

impl embedded_hal_async::i2c::Error for Error {
    fn kind(&self) -> embedded_hal_async::i2c::ErrorKind {
        embedded_hal_async::i2c::ErrorKind::Other
    }
}

impl embedded_hal_async::i2c::ErrorType for I2c {
    type Error = Error;
}

impl embedded_hal_async::i2c::I2c for I2c {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [embedded_hal_async::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        if operations.len() > MAX_MSGS {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many i2c operations in one transaction",
            )));
        }

        // SAFETY: I2cMsg is repr(C) with all integer fields and a
        // raw pointer; zero-init produces null pointers, which are
        // overwritten before the kernel sees the array.
        let mut msgs: [I2cMsg; MAX_MSGS] = unsafe { core::mem::zeroed() };
        for (slot, operation) in msgs.iter_mut().zip(operations.iter_mut()) {
            match operation {
                embedded_hal_async::i2c::Operation::Read(buffer) => {
                    *slot = I2cMsg {
                        addr: u16::from(address),
                        flags: I2C_M_RD,
                        len: buffer.len() as u16,
                        buf: buffer.as_mut_ptr(),
                    };
                }
                embedded_hal_async::i2c::Operation::Write(buffer) => {
                    *slot = I2cMsg {
                        addr: u16::from(address),
                        flags: 0,
                        len: buffer.len() as u16,
                        // The kernel does not modify write buffers,
                        // but the struct field is non-const.
                        buf: buffer.as_ptr().cast_mut(),
                    };
                }
            }
        }

        let mut header = I2cRdwrIoctlData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: operations.len() as u32,
        };

        // We execute the ioctl synchronously. In a host testing environment,
        // blocking the embassy executor for the duration of a local I2C
        // transaction (~1ms) is perfectly fine and avoids the epoll EPERM trap.
        let result = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                I2C_RDWR,
                std::ptr::addr_of_mut!(header),
            )
        };

        if result < 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }

        Ok(())
    }
}
