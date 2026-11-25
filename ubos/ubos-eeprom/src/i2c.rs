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

//! I2C-backed [`HashboardEeprom`] for the M24C02-class serial EEPROMs
//! the factory layout uses. Their write pages are 16 bytes but reads
//! are not page limited, so the whole chip comes back in one
//! transaction.

use embedded_hal_async::i2c::I2c;

use crate::HashboardEeprom;
use crate::forge::{RECORD_LEN, ReadError, Record};

#[cfg(test)]
#[path = "tests/i2c.rs"]
mod tests;

/// One EEPROM chip on the I2C bus. The bus is borrowed for as long as the
/// reader is in scope, because a one-shot boot-time read needs none of the
/// `'static` shared-mutex plumbing a long-running driver does.
pub struct I2cEeprom<'bus, I> {
    address: u8,
    bus: &'bus mut I,
}

impl<'bus, I: I2c> I2cEeprom<'bus, I> {
    #[must_use]
    pub fn new(address: u8, bus: &'bus mut I) -> Self {
        Self { address, bus }
    }

    /// Read the Forge record of the EEPROM into `raw`, then parse it.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError::I2c`] when the EEPROM does not answer, and
    /// [`ReadError::Record`] when its bytes hold no record.
    pub async fn read_record<'a>(
        &mut self,
        raw: &'a mut [u8; RECORD_LEN],
    ) -> Result<Record<'a>, ReadError<I::Error>> {
        self.read(raw).await.map_err(ReadError::I2c)?;
        let raw: &'a [u8; RECORD_LEN] = raw;
        Record::parse(raw).map_err(ReadError::Record)
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
