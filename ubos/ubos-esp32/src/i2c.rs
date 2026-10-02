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

//! Shared I2C0 bus type for the control board. The board owns the mutex and
//! every consumer shares it through the [`Device`] handle named here.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::i2c::master::I2c;

/// Bus mutex shared by every consumer on I2C0. `NoopRawMutex` is sufficient
/// because every I2C consumer runs on the Core 0 executor. The chip serial
/// worker runs on Core 1 and does not touch I2C, so there is no cross-core
/// contention. A board that moves an I2C consumer to Core 1 must change
/// this to `CriticalSectionRawMutex`, since nothing gates the claim.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, esp_hal::Async>>;

/// `embedded-hal-async` handle for one consumer of the shared bus.
pub type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, esp_hal::Async>>;
