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
//! low holds the chain in reset, high releases it. The other seven lines
//! belong to whatever else the carrier wires to the expander, so every
//! write here changes this pin's bit alone and leaves the rest as the
//! device last reported them.

use defmt::warn;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use tca9554::{Address, Tca9554};

use crate::i2c::{Bus, Device};

/// The direction register's power-on value: every line an input.
const ALL_INPUTS: u8 = 0xFF;

/// Drives a hashboard reset line through the carrier's TCA9554 I/O
/// expander on the given pin.
pub struct ExpanderResetPin {
    expander: Tca9554<Device>,
    pin_bit: u8,
    /// The output register as last written, so a transition can change
    /// this pin's bit and nothing else. Seeded from the device in `new`.
    outputs: u8,
}

impl ExpanderResetPin {
    /// Carve an expander handle from the shared I2C bus, configure the
    /// given pin as an output and assert reset by driving it low, so the
    /// miner task controls release timing. A failed expander access is
    /// logged and ignored, per [`ubos_mining::reset::ResetPin`].
    pub async fn new(bus: &'static Bus, pin_bit: u8) -> Self {
        let mut expander = Tca9554::new(I2cDevice::new(bus), Address::standard());
        // Both registers are read back first so the other lines keep
        // whatever state the carrier gave them. The pin is driven low
        // before it is switched to an output: the output latch powers up
        // high, so writing the direction first would briefly drive the
        // pin high and deassert reset before the output write lands.
        let outputs = expander.read_output().await.unwrap_or(0) & !pin_bit;
        let direction = expander.read_direction().await.unwrap_or(ALL_INPUTS) & !pin_bit;
        if expander.write_output(outputs).await.is_err()
            || expander.write_direction(direction).await.is_err()
        {
            warn!("expander: TCA9554 reset-pin init failed");
        }
        Self {
            expander,
            pin_bit,
            outputs,
        }
    }

    /// Write the port with this pin's bit set to `high` and the other
    /// bits as last written. The shadow follows the device only on a
    /// successful write, so a failed transition is retried from the
    /// state the device still holds.
    async fn set_pin(&mut self, high: bool, what: &str) {
        let outputs = if high {
            self.outputs | self.pin_bit
        } else {
            self.outputs & !self.pin_bit
        };
        if self.expander.write_output(outputs).await.is_err() {
            warn!("expander: TCA9554 {} write failed", what);
            return;
        }
        self.outputs = outputs;
    }
}

impl ubos_mining::reset::ResetPin for ExpanderResetPin {
    async fn enter_reset(&mut self) {
        self.set_pin(false, "enter_reset").await;
    }

    async fn exit_reset(&mut self) {
        self.set_pin(true, "exit_reset").await;
    }
}
