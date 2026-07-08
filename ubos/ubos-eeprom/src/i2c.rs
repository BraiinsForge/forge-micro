//! I2C-backed [`HashboardEeprom`]. Targets the M24C02-class serial
//! EEPROMs `bos-main` uses: 256 bytes, 16-byte write pages, but reads
//! are not page-limited, so the whole chip comes back in one
//! transaction. A future 1-Wire chip would live in its own sibling
//! module implementing the same trait.

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
