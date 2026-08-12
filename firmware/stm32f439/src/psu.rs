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

//! BMM101 PSU integration over the shared I2C1 bus.
//!
//! Owns the static cells for the `BmmPsu` state, the terminated flag,
//! and the PSU handle, all of which `ubos_psu::i2c_psu!` writes. The bus
//! mutex is owned by `main`, so another consumer takes its own
//! `I2cDevice` from the same mutex rather than one of these; what it
//! does take from here is the [`Device`] alias that names the handle
//! type.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_stm32::i2c::{I2c, Master};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because the STM32F439 is single-core: every I2C consumer runs on the same
/// executor and there is no cross-core contention.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
/// Cloning the bus reference is the supported shared-bus pattern. Public
/// because the hashboard sensor array takes one of these too.
pub type Device =
    I2cDevice<'static, NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

ubos_psu::i2c_psu!(Bus, Device, crate::config::MACHINE.psu);
