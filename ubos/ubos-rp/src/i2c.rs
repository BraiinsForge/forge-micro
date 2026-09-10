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

//! An `embassy_rp::gpio::Flex` pad adapter for `ubos_core::i2c::bus_clear`.
//!
//! `bus_clear_flex` is free rather than a method because `Flex` is
//! `embassy-rp`'s type and the adapter newtype below is private.

use embassy_rp::gpio::Flex;
use embassy_time::Delay;
use ubos_core::i2c::{BusClearError, OpenDrainPin, bus_clear};

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
