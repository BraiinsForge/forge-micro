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

//! LM75A / LM75B I2C temperature sensor driver.
//!
//! Reads the 11-bit two's-complement temperature register at 0x00 and
//! converts it to a [`Temperature`]. Resolution is 0.125°C on the LM75B and
//! 0.5°C on the LM75A. Both produce the same register layout, so the
//! driver handles either.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::units::Temperature;

use crate::{Readings, TempSource, add_source};

#[cfg(test)]
#[path = "tests/lm75a.rs"]
mod tests;

const REG_TEMPERATURE: u8 = 0x00;

/// Datasheet supported range, exclusive of garbage readings on bus
/// errors that produce e.g. `0xff80`, which decodes to a plausible
/// −1°C, or `0x7fff`. The chip itself only reports −55..=125°C.
const MIN_TEMPERATURE: Temperature = Temperature::from_millicelsius(-55_000);
const MAX_TEMPERATURE: Temperature = Temperature::from_millicelsius(125_000);

/// One LM75A on the I2C bus: its address and what it measures. The bus
/// is passed in on each read so multiple sensors can share a `&mut I`
/// without owning it.
#[derive(Clone, Copy)]
pub struct Lm75a {
    /// The sensor's 7-bit I2C address.
    pub address: SevenBitAddress,
    /// What the sensor's own channel measures.
    pub local: TempSource,
}

impl Lm75a {
    /// Read the temperature register.
    /// Returns `Err` on bus error or out-of-range reading.
    pub async fn read_temperature<I, E>(&self, i2c: &mut I) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let mut buffer = [0u8; 2];
        i2c.write_read(self.address, &[REG_TEMPERATURE], &mut buffer)
            .await
            .map_err(Error::I2c)?;
        let temperature: Temperature = RawLm75a(buffer).into();
        if !(MIN_TEMPERATURE..=MAX_TEMPERATURE).contains(&temperature) {
            return Err(Error::OutOfRange(temperature));
        }
        Ok(temperature)
    }

    /// Read the temperature as what [`local`](Self::local) names, or the
    /// fault that kept it from one.
    pub async fn read_temperatures<I: I2c>(&self, i2c: &mut I) -> Readings<I::Error> {
        [add_source(self.local, self.read_temperature(i2c).await)]
            .into_iter()
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    I2c(E),
    OutOfRange(Temperature),
}

/// The 16-bit big-endian temperature register as read from the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawLm75a(pub(crate) [u8; 2]);

/// The high byte is signed integer degrees C, and the low byte's top 3
/// bits are fractional in 0.125°C steps. The result is exact
/// millicelsius, computed without floats.
///
/// The low byte's bottom 5 bits are reserved on the LM75A/B; some
/// clones leave noise there instead of zeros. They are masked before the
/// shift so a misbehaving part cannot leak garbage into the result even
/// if the sign-extension semantics of `>>` were to surprise us on a
/// future toolchain.
impl From<RawLm75a> for Temperature {
    fn from(raw: RawLm75a) -> Self {
        let masked = [raw.0[0], raw.0[1] & 0xE0];
        let eighths = i16::from_be_bytes(masked) >> 5;
        Self::from_millicelsius(i32::from(eighths) * 125)
    }
}
