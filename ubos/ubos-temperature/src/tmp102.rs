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

//! TMP102 I2C temperature sensor driver.
//!
//! The part powers on in continuous-conversion mode at 4 Hz with 12-bit
//! resolution, so a read-only user needs no configuration write.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::index::HashboardIndex;
use ubos_core::units::Temperature;

use crate::{Readings, TempSource, add_source};

#[cfg(test)]
#[path = "tests/tmp102.rs"]
mod tests;

/// Power-on reset selects this register, so a plain two-byte read also
/// works while nothing has moved the pointer.
const REG_TEMPERATURE: u8 = 0x00;

/// The part measures −40..125 °C. The window is wider on both sides, so a
/// bus glitch that decodes to an impossible temperature is caught and a
/// valid near-limit reading is not.
const MIN_TEMPERATURE: Temperature = Temperature::from_millicelsius(-55_000);
const MAX_TEMPERATURE: Temperature = Temperature::from_millicelsius(150_000);

/// LSB is 0.0625 °C = 62.5 m°C, encoded as 125/2 to stay in integer math.
const MILLICELSIUS_NUMERATOR: i32 = 125;
const MILLICELSIUS_DENOMINATOR: i32 = 2;

#[derive(Clone, Copy)]
pub struct Tmp102 {
    pub(crate) address: SevenBitAddress,
    /// What the sensor's own channel measures.
    pub(crate) local: TempSource,
}

impl Tmp102 {
    /// The sensor at `address` that measures the hashboard on `chain`.
    /// The source of its readings carries the same address, so a board
    /// states it once.
    #[must_use]
    pub const fn hashboard(chain: u8, address: SevenBitAddress) -> Self {
        let chain = HashboardIndex(chain);
        Self {
            address,
            local: TempSource::Hashboard { chain, address },
        }
    }

    /// The sensor at `address` that measures the power supply.
    #[must_use]
    pub const fn psu(address: SevenBitAddress) -> Self {
        Self {
            address,
            local: TempSource::Psu,
        }
    }

    /// What the sensor's own channel measures.
    #[must_use]
    pub const fn local(&self) -> TempSource {
        self.local
    }

    pub async fn read_temperature<I, E>(&self, i2c: &mut I) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let mut buffer = [0u8; 2];
        i2c.write_read(self.address, &[REG_TEMPERATURE], &mut buffer)
            .await
            .map_err(Error::I2c)?;
        let temperature: Temperature = RawTmp102(buffer).into();
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
pub(crate) struct RawTmp102(pub(crate) [u8; 2]);

/// The 12-bit value sits left-justified in the 16-bit word, so the
/// arithmetic shift sign-extends it and discards the four padding bits,
/// which are zero in 12-bit mode.
impl From<RawTmp102> for Temperature {
    fn from(raw: RawTmp102) -> Self {
        let raw12 = i16::from_be_bytes(raw.0) >> 4;
        Self::from_millicelsius(
            i32::from(raw12) * MILLICELSIUS_NUMERATOR / MILLICELSIUS_DENOMINATOR,
        )
    }
}
