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

//! A generic I2C bus clear over open-drain-style pins.
//!
//! A target left mid-transfer holds SDA low and a controller's own bus
//! driver wedges on its first transfer, waiting for an idle bus that never
//! comes. This clocks SCL until the target releases SDA, then issues a
//! STOP so every target's state machine returns to idle. It runs before a
//! platform's I2C driver claims the pins.
//!
//! Every line is driven open-drain style: asserted low, then released back
//! to the bus pull-up rather than driven high, so a target holding a line
//! low is never fought electrically. [`OpenDrainPin`] is the pin shape this
//! needs: a plain `embedded-hal` pin cannot express "drive low, then
//! release to input" on a single pin, since `OutputPin` and `InputPin` are
//! two different modes of the same pad on a HAL like `Flex`.
//!
//! `bus_clear` is a free function: it drives two pins of equal standing,
//! and both are generic parameters this crate cannot hang a method on.

use embedded_hal::delay::DelayNs;

#[cfg(test)]
#[path = "tests/i2c.rs"]
mod tests;

/// Clock pulses a stuck-low SDA needs to walk a confused target through the
/// rest of its byte plus the ACK slot.
const RECOVERY_PULSES: u8 = 9;
/// Half-period of the recovery clock, near 100 kHz. It meets I2C
/// standard-mode's high and low minimums; the exact rate is otherwise
/// uncritical, since the target shifts on the edges we drive.
const RECOVERY_HALF_PERIOD_US: u32 = 5;

/// One I2C line, released to the bus pull-up or driven low, with the
/// asserted-low direction being push-pull rather than a fight against a
/// target holding the line.
///
/// A HAL whose pin type switches between input and output mode, such as
/// `embassy_rp::gpio::Flex`, implements this with `release` as
/// `set_as_input` and `drive_low` as `set_low` followed by `set_as_output`.
pub trait OpenDrainPin {
    fn release(&mut self);
    fn drive_low(&mut self);
    fn is_high(&mut self) -> bool;
}

/// Why [`bus_clear`] could not free the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusClearError {
    /// SCL read low at idle, before any clocking. No amount of clocking
    /// frees a bus whose clock line itself is stuck, so the caller's own
    /// wiring or the target's power rail needs checking first.
    SclStuckLow,
    /// SDA was still low after the recovery pulses.
    SdaStillLow,
}

/// Free a hung I2C bus before the controller takes the pads.
///
/// Returns [`BusClearError::SclStuckLow`] without clocking if SCL reads
/// low at idle, since driving SCL would only contend with whatever holds
/// it. Otherwise it ends with a STOP, so a target that is mid-transfer
/// without holding SDA low is returned to idle too.
///
/// # Errors
///
/// See [`BusClearError`].
pub fn bus_clear<Scl, Sda, D>(
    scl: &mut Scl,
    sda: &mut Sda,
    delay: &mut D,
) -> Result<(), BusClearError>
where
    Scl: OpenDrainPin,
    Sda: OpenDrainPin,
    D: DelayNs,
{
    scl.release();
    sda.release();
    delay.delay_us(RECOVERY_HALF_PERIOD_US);

    if !scl.is_high() {
        return Err(BusClearError::SclStuckLow);
    }
    if !sda.is_high() {
        for _ in 0..RECOVERY_PULSES {
            scl.drive_low();
            delay.delay_us(RECOVERY_HALF_PERIOD_US);
            scl.release();
            delay.delay_us(RECOVERY_HALF_PERIOD_US);
            if sda.is_high() {
                break;
            }
        }
    }

    sda.drive_low();
    delay.delay_us(RECOVERY_HALF_PERIOD_US);
    sda.release();
    delay.delay_us(RECOVERY_HALF_PERIOD_US);

    if sda.is_high() {
        Ok(())
    } else {
        Err(BusClearError::SdaStillLow)
    }
}
