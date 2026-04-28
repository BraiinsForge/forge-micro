//! Async I2C adapter for Linux `/dev/i2c-N` character devices.
//!
//! Mirrors `embedded-serial-unix`: open a non-blocking fd, wrap
//! it in `async_io::Async`, expose
//! `embedded_hal_async::i2c::I2c` over `I2C_RDWR` ioctls.

#![warn(missing_docs)]
