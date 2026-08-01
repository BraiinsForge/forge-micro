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

//! I2C-backed [`HashboardEeprom`]. Targets the M24C02-class serial
//! EEPROMs BOS uses: 256 bytes, 16-byte write pages, but reads are not
//! page-limited, so the whole chip comes back in one transaction. A
//! future 1-Wire chip would live in its own sibling module implementing
//! the same trait.

use embedded_hal_async::i2c::I2c;

use crate::HashboardEeprom;

#[cfg(test)]
#[path = "tests/i2c.rs"]
mod tests;

/// One EEPROM chip on the I2C bus, address fixed at construction and
/// the bus borrowed for as long as the reader is in scope — a one-shot
/// boot-time read has no need for the `'static`-shared-mutex plumbing
/// `ubos_psu`'s long-running drivers use.
pub struct I2cEeprom<'bus, I> {
    address: u8,
    bus: &'bus mut I,
}

impl<'bus, I: I2c> I2cEeprom<'bus, I> {
    #[must_use]
    pub fn new(address: u8, bus: &'bus mut I) -> Self {
        Self { address, bus }
    }
}

impl<I: I2c> HashboardEeprom for I2cEeprom<'_, I> {
    type Error = I::Error;

    /// A one-byte "set the address pointer to 0" write followed by a
    /// sequential read, which every I2C serial EEPROM supports
    /// regardless of its write page size.
    async fn read(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.bus.write_read(self.address, &[0], buf).await
    }
}
