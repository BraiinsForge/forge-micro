//! Async PSU driver for the BMM100 miniminer hashboard.
//!
//! Generic over `embedded_hal_async::i2c::I2c`. Runs unchanged on
//! Linux (via `embedded-i2c-unix`) and on rp2040/rp2350 (via
//! `embassy-rp::i2c`).

#![no_std]
