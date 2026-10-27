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

//! Shared I2C1 bus types for the control board.
//!
//! The board owns the bus mutex and every consumer, the PSU and the
//! temperature sensor array among them, shares it through the [`Device`]
//! handle named here rather than spelling out the same type once per board.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because both STM32 boards are single-core: every I2C consumer runs on the
/// same executor and there is no cross-core contention.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, Async, Master>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
/// Cloning the bus reference is the supported shared-bus pattern.
pub type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, Async, Master>>;
