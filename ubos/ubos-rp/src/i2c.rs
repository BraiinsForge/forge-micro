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

//! The shared I2C1 bus types, and an `embassy_rp::gpio::Flex` pad adapter
//! for `ubos_core::i2c::bus_clear`.
//!
//! The board owns the bus mutex and every consumer, the PSU and the
//! temperature sensor array among them, shares it through the [`Device`]
//! handle named here rather than spelling out the same type once per board.
//!
//! `bus_clear_flex` is free rather than a method because `Flex` is
//! `embassy-rp`'s type and the adapter newtype below is private.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::gpio::Flex;
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Delay;
use ubos_core::i2c::{BusClearError, OpenDrainPin, bus_clear};

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because every I2C consumer runs on the Core 0 executor. The chip serial
/// worker runs on Core 1 and does not touch I2C, so there is no cross-core
/// contention. A board that moves an I2C consumer to Core 1 must change
/// this to `CriticalSectionRawMutex`, since nothing gates the claim.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, I2C1, Async>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
/// Cloning the bus reference is the supported shared-bus pattern.
pub type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

/// Wraps a `Flex` pad so it implements [`OpenDrainPin`]: `release` sets the
/// pad back to an input, letting the bus pull-up hold the line high, and
/// `drive_low` drives it low as a push-pull output. The pad's pull must
/// already be configured by the caller; this adapter only switches
/// direction.
struct FlexPad<'a, 'd>(&'a mut Flex<'d>);

impl OpenDrainPin for FlexPad<'_, '_> {
    fn release(&mut self) {
        self.0.set_as_input();
    }

    fn drive_low(&mut self) {
        self.0.set_low();
        self.0.set_as_output();
    }

    fn is_high(&mut self) -> bool {
        self.0.is_high()
    }
}

/// Free a hung I2C bus over two `Flex` pads before the board's I2C driver
/// claims the pins. `scl` and `sda` must already carry the pull the bus
/// needs; this call only switches their direction.
///
/// # Errors
///
/// See [`ubos_core::i2c::bus_clear`].
pub fn bus_clear_flex(scl: &mut Flex<'_>, sda: &mut Flex<'_>) -> Result<(), BusClearError> {
    let mut scl_pad = FlexPad(scl);
    let mut sda_pad = FlexPad(sda);
    let mut delay = Delay;
    bus_clear(&mut scl_pad, &mut sda_pad, &mut delay)
}
