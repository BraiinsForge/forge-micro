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

//! Async serial driver for Linux character devices.
//!
//! The fd is non-blocking and wrapped in `async_io::Async`, so a waiting
//! task parks instead of busy-polling. A retune through
//! [`serial_baudrate::BaudRate`] acts on the underlying tty, so it
//! applies to both directions.

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

use async_io::Async;

/// Read and write take `&self`, because `libc::read` and `libc::write`
/// need no Rust-level exclusive access.
struct Fd(RawFd);

impl Fd {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = unsafe {
            libc::read(
                self.0,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if length == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(length as usize)
    }

    fn write(&self, buffer: &[u8]) -> io::Result<usize> {
        let length =
            unsafe { libc::write(self.0, buffer.as_ptr().cast::<libc::c_void>(), buffer.len()) };
        if length == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(length as usize)
    }
}

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Returns `InvalidInput` for any rate outside the kernel's `Bxxxx`
/// table.
fn baud_to_speed(baud: u32) -> io::Result<libc::speed_t> {
    let speed = match baud {
        50 => libc::B50,
        75 => libc::B75,
        110 => libc::B110,
        134 => libc::B134,
        150 => libc::B150,
        200 => libc::B200,
        300 => libc::B300,
        600 => libc::B600,
        1200 => libc::B1200,
        1800 => libc::B1800,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115_200 => libc::B115200,
        230_400 => libc::B230400,
        460_800 => libc::B460800,
        500_000 => libc::B500000,
        576_000 => libc::B576000,
        921_600 => libc::B921600,
        1_000_000 => libc::B1000000,
        1_152_000 => libc::B1152000,
        1_500_000 => libc::B1500000,
        2_000_000 => libc::B2000000,
        2_500_000 => libc::B2500000,
        3_000_000 => libc::B3000000,
        3_500_000 => libc::B3500000,
        4_000_000 => libc::B4000000,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported baud rate: {baud}"),
            ));
        }
    };
    Ok(speed)
}

/// Initial termios setup: 8N1 raw mode at the requested baud.
fn configure(fd: RawFd, baud: u32) -> io::Result<()> {
    let speed = baud_to_speed(baud)?;
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, std::ptr::from_mut(&mut termios)) } == -1 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::cfmakeraw(std::ptr::from_mut(&mut termios)) };
    if unsafe { libc::cfsetispeed(std::ptr::from_mut(&mut termios), speed) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::cfsetospeed(std::ptr::from_mut(&mut termios), speed) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, std::ptr::from_ref(&termios)) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reads the current termios first, so the rest of the line settings
/// survive the change.
fn apply_baud(fd: RawFd, baud: u32) -> io::Result<()> {
    let speed = baud_to_speed(baud)?;
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, std::ptr::from_mut(&mut termios)) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::cfsetispeed(std::ptr::from_mut(&mut termios), speed) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::cfsetospeed(std::ptr::from_mut(&mut termios), speed) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, std::ptr::from_ref(&termios)) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Duplex handle over a Linux serial device.
pub struct Serial {
    inner: Async<Fd>,
}

impl Serial {
    /// Open `path` in 8N1 raw mode at the requested `baud`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` if `baud` is not in the supported
    /// table, and the underlying `io::Error` if `open`, any of the
    /// termios calls, or async-io registration fail.
    pub fn open(path: &str, baud: u32) -> io::Result<Self> {
        let cstring = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let flags = libc::O_RDWR | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
        let fd = unsafe { libc::open(cstring.as_ptr(), flags) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        // From here on, the `Fd` wrapper owns the descriptor and
        // closes it if any subsequent step fails.
        let owned = Fd(fd);
        configure(owned.as_raw_fd(), baud)?;
        let inner = Async::new(owned)?;
        Ok(Self { inner })
    }
}

/// Logs the real error, because the [`embedded_io::ErrorKind`] it
/// returns keeps nothing but the taxonomy.
fn map_set_baud_err(error: &io::Error, baud: u32) -> embedded_io::ErrorKind {
    log::error!("set_baudrate({baud}) failed: {error}");
    if error.kind() == io::ErrorKind::InvalidInput {
        embedded_io::ErrorKind::Unsupported
    } else {
        embedded_io::ErrorKind::Other
    }
}

impl serial_baudrate::BaudRate for Serial {
    fn set_baudrate(&mut self, baud: u32) -> Result<(), embedded_io::ErrorKind> {
        apply_baud(self.inner.get_ref().as_raw_fd(), baud).map_err(|e| map_set_baud_err(&e, baud))
    }
}

impl embedded_io_async::ErrorType for Serial {
    type Error = io::Error;
}

impl embedded_io_async::Read for Serial {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read_with(|fd| fd.read(buf)).await
    }
}

impl embedded_io_async::Write for Serial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write_with(|fd| fd.write(buf)).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // Kernel TX buffers drain on their own, and `tcdrain` would block
        // the async path until the UART hardware empties.
        Ok(())
    }
}
