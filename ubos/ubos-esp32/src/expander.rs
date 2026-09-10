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

//! Reset pin driven through the carrier's TCA9554 I/O expander.
//!
//! One line of the expander holds a hashboard's reset signal. Active-low:
//! low holds the chain in reset, high releases it.

use defmt::warn;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use tca9554::{Address, Tca9554};

use crate::i2c::{Bus, Device};

/// Drives a hashboard reset line through the carrier's TCA9554 I/O
/// expander on the given pin.
pub struct ExpanderResetPin {
    expander: Tca9554<Device>,
    pin_bit: u8,
}

impl ExpanderResetPin {
    /// Carve an expander handle from the shared I2C bus, configure the
    /// given pin as an output and assert reset by driving it low, so the
    /// miner task controls release timing. A failed expander access is
    /// logged and ignored, per [`ubos_mining::reset::ResetPin`].
    pub async fn new(bus: &'static Bus, pin_bit: u8) -> Self {
        let mut expander = Tca9554::new(I2cDevice::new(bus), Address::standard());
        // The pin is driven low before it is switched to an output. The
        // TCA9554 output latch powers up at 0xFF, so writing the direction
        // first would briefly drive the pin high and deassert reset before
        // the output write lands.
        // Output register: 0 = low on every pin. Direction register: 0 = output.
        if expander.write_output(0).await.is_err()
            || expander.write_direction(!pin_bit).await.is_err()
        {
            warn!("expander: TCA9554 reset-pin init failed");
        }
        Self { expander, pin_bit }
    }
}

impl ubos_mining::reset::ResetPin for ExpanderResetPin {
    async fn enter_reset(&mut self) {
        if self.expander.write_output(0).await.is_err() {
            warn!("expander: TCA9554 enter_reset write failed");
        }
    }

    async fn exit_reset(&mut self) {
        if self.expander.write_output(self.pin_bit).await.is_err() {
            warn!("expander: TCA9554 exit_reset write failed");
        }
    }
}
