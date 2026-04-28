//! Async serial driver for Linux character devices.
//!
//! Opens `/dev/tty*` or any tty-like character device in 8N1 raw mode
//! at a caller-supplied baud rate, then exposes async I/O via the
//! `embedded-io-async` traits. The duplex handle splits into
//! independent [`Rx`]/[`Tx`] halves backed by the same shared file
//! descriptor; [`BaudRate`] lets either half retune the line, with
//! the change applying to both directions because the underlying tty
//! is shared.
//!
//! The shape mirrors `embassy-net-afpacket`: open a non-blocking fd,
//! wrap it in `async_io::Async`, and let the async-io reactor handle
//! readiness so the calling task parks instead of busy-waiting.

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
use std::sync::Arc;

use async_io::Async;

/// Owned file descriptor that closes on drop. Read and write take
/// `&self` because `libc::read`/`libc::write` do not require
/// Rust-level exclusive access; this lets the split halves share the
/// fd via `Arc<Async<Fd>>` without a mutex.
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

/// Map a numeric baud rate to the kernel's `Bxxxx` `speed_t` constant.
/// Returns `InvalidInput` for any rate not in the standard table.
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

/// Reverse of [`baud_to_speed`]. Returns `InvalidData` if the tty was
/// configured by another process to a rate outside the standard
/// table.
fn speed_to_baud(speed: libc::speed_t) -> io::Result<u32> {
    let baud = match speed {
        libc::B50 => 50,
        libc::B75 => 75,
        libc::B110 => 110,
        libc::B134 => 134,
        libc::B150 => 150,
        libc::B200 => 200,
        libc::B300 => 300,
        libc::B600 => 600,
        libc::B1200 => 1200,
        libc::B1800 => 1800,
        libc::B2400 => 2400,
        libc::B4800 => 4800,
        libc::B9600 => 9600,
        libc::B19200 => 19200,
        libc::B38400 => 38400,
        libc::B57600 => 57600,
        libc::B115200 => 115_200,
        libc::B230400 => 230_400,
        libc::B460800 => 460_800,
        libc::B500000 => 500_000,
        libc::B576000 => 576_000,
        libc::B921600 => 921_600,
        libc::B1000000 => 1_000_000,
        libc::B1152000 => 1_152_000,
        libc::B1500000 => 1_500_000,
        libc::B2000000 => 2_000_000,
        libc::B2500000 => 2_500_000,
        libc::B3000000 => 3_000_000,
        libc::B3500000 => 3_500_000,
        libc::B4000000 => 4_000_000,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "termios reports a baud rate not in the supported table",
            ));
        }
    };
    Ok(baud)
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

/// Apply only the baud rate to an already-configured fd. Reads the
/// current termios first so the rest of the line settings are
/// preserved.
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

fn current_baud(fd: RawFd) -> io::Result<u32> {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, std::ptr::from_mut(&mut termios)) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let speed = unsafe { libc::cfgetospeed(std::ptr::from_ref(&termios)) };
    speed_to_baud(speed)
}

/// Duplex handle over a Linux serial device. Construct with
/// [`Serial::open`], then call [`Serial::split`] to get independent
/// reader and writer halves.
pub struct Serial {
    inner: Arc<Async<Fd>>,
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
        let inner = Arc::new(Async::new(owned)?);
        Ok(Self { inner })
    }

    /// Split into independent reader and writer halves. Both halves
    /// share the same underlying fd, so [`BaudRate`] changes on
    /// either side affect both.
    #[must_use]
    pub fn split(self) -> (Rx, Tx) {
        let rx = Rx {
            inner: Arc::clone(&self.inner),
        };
        let tx = Tx { inner: self.inner };
        (rx, tx)
    }
}

/// Read half of a [`Serial`]. Implements [`embedded_io_async::Read`].
pub struct Rx {
    inner: Arc<Async<Fd>>,
}

/// Write half of a [`Serial`]. Implements [`embedded_io_async::Write`].
pub struct Tx {
    inner: Arc<Async<Fd>>,
}

/// Get or set the line's baud rate. Implemented for [`Serial`],
/// [`Rx`], and [`Tx`]; calling on any of them updates the shared tty.
pub trait BaudRate {
    /// Apply `baud` to the line. The change is immediate
    /// (`TCSANOW`) and visible to both halves.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` if `baud` is not in the supported
    /// table, and the underlying `io::Error` for termios failures.
    fn set_baudrate(&self, baud: u32) -> io::Result<()>;

    /// Read back the line's current baud rate.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` if the kernel reports a rate outside
    /// the standard table, and the underlying `io::Error` for
    /// termios failures.
    fn baudrate(&self) -> io::Result<u32>;
}

impl BaudRate for Serial {
    fn set_baudrate(&self, baud: u32) -> io::Result<()> {
        apply_baud(self.inner.get_ref().as_raw_fd(), baud)
    }
    fn baudrate(&self) -> io::Result<u32> {
        current_baud(self.inner.get_ref().as_raw_fd())
    }
}

impl BaudRate for Rx {
    fn set_baudrate(&self, baud: u32) -> io::Result<()> {
        apply_baud(self.inner.get_ref().as_raw_fd(), baud)
    }
    fn baudrate(&self) -> io::Result<u32> {
        current_baud(self.inner.get_ref().as_raw_fd())
    }
}

impl BaudRate for Tx {
    fn set_baudrate(&self, baud: u32) -> io::Result<()> {
        apply_baud(self.inner.get_ref().as_raw_fd(), baud)
    }
    fn baudrate(&self) -> io::Result<u32> {
        current_baud(self.inner.get_ref().as_raw_fd())
    }
}

impl embedded_io_async::ErrorType for Rx {
    type Error = io::Error;
}

impl embedded_io_async::Read for Rx {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read_with(|fd| fd.read(buf)).await
    }
}

impl embedded_io_async::ErrorType for Tx {
    type Error = io::Error;
}

impl embedded_io_async::Write for Tx {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write_with(|fd| fd.write(buf)).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // Kernel TX buffers drain on their own. `tcdrain` would block
        // until the UART hardware empties; not worth a synchronous
        // syscall in an async path.
        Ok(())
    }
}
