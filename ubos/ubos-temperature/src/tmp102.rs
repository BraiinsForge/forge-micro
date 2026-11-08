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
//! Reads the 12-bit two's-complement temperature register at 0x00 and
//! converts to millicelsius. The TMP102 powers on in continuous-conversion
//! mode at 4 Hz with 12-bit resolution; no configuration write is needed
//! for a simple read-only use.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::units::Temperature;

use crate::{Readings, TempSource, add_source};

#[cfg(test)]
#[path = "tests/tmp102.rs"]
mod tests;

/// Temperature register pointer. POR value selects this register, so a
/// plain two-byte read without a preceding pointer write also works if the
/// pointer register has not been changed.
const REG_TEMPERATURE: u8 = 0x00;

/// Datasheet supported range. The TMP102 measures −40..125 °C; the accepted
/// window adds 15 °C below and 25 °C above to catch bus glitches that decode
/// to plausible but impossible temperatures without rejecting valid
/// near-limit readings.
const MIN_TEMPERATURE: Temperature = Temperature::from_millicelsius(-55_000);
const MAX_TEMPERATURE: Temperature = Temperature::from_millicelsius(150_000);

/// LSB is 0.0625 °C = 62.5 m°C, encoded as 125/2 to stay in integer math.
const MILLICELSIUS_NUMERATOR: i32 = 125;
const MILLICELSIUS_DENOMINATOR: i32 = 2;

/// One TMP102 on the I2C bus: its address and what it measures. The bus
/// is passed in on each read so multiple sensors can share a `&mut I`
/// without each owning a copy.
#[derive(Clone, Copy)]
pub struct Tmp102 {
    /// The sensor's 7-bit I2C address.
    pub address: SevenBitAddress,
    /// What the sensor's own channel measures.
    pub local: TempSource,
}

impl Tmp102 {
    /// Read the temperature register.
    ///
    /// Returns `Err` on a bus error or when the decoded value is outside
    /// the datasheet operating range.
    pub async fn read_temperature<I, E>(&self, i2c: &mut I) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let mut buffer = [0u8; 2];
        i2c.write_read(self.address, &[REG_TEMPERATURE], &mut buffer)
            .await
            .map_err(Error::I2c)?;
        let temperature = Temperature::from_millicelsius(raw_to_millicelsius(buffer));
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

/// Error returned by [`Tmp102::read_temperature`].
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// I2C bus error.
    I2c(E),
    /// Decoded value is outside the datasheet operating range.
    OutOfRange(Temperature),
}

/// Decode the 16-bit big-endian register to millicelsius.
///
/// The TMP102 stores the 12-bit value left-justified in a 16-bit word.
/// The arithmetic right-shift by 4 sign-extends the twos-complement value
/// while discarding the four padding bits in bits 3:0 (which the datasheet
/// states as zero in 12-bit mode). LSB = 0.0625 °C = 62.5 m°C; multiply
/// by 125 and divide by 2 stays in i32 with no intermediate overflow across
/// the full ±128 °C datasheet range.
pub(crate) fn raw_to_millicelsius(buffer: [u8; 2]) -> i32 {
    let raw12 = i16::from_be_bytes(buffer) >> 4;
    i32::from(raw12) * MILLICELSIUS_NUMERATOR / MILLICELSIUS_DENOMINATOR
}
