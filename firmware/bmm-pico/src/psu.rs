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

//! BMM101 PSU integration over the shared I2C1 bus. Owns the static
//! cells for the shared `BmmPsu` state and the PSU handle.
//!
//! Backend architecture: `../../../docs/MINING.md`.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because every I2C consumer runs on the Core 0 executor. The chip-serial
/// actor (`mining::serial_task`) runs on Core 1 but does not touch I2C, so
/// there is no cross-core contention. If a future I2C consumer moves to
/// Core 1, bump this to `CriticalSectionRawMutex`.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, I2C1, Async>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
/// Cloning the bus reference is the supported shared-bus pattern. Public
/// because the hashboard sensor array takes one of these too.
pub type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

ubos_psu::bmm_psu!(Bus, Device, crate::config::MACHINE.psu);
