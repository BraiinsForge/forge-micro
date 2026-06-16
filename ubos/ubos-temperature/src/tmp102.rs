//! TMP102 I2C temperature sensor driver.
//!
//! Reads the 12-bit two's-complement temperature register at 0x00 and
//! converts to millicelsius. The TMP102 powers on in continuous-conversion
//! mode at 4 Hz with 12-bit resolution; no configuration write is needed
//! for a simple read-only use.

use embedded_hal_async::i2c::I2c;

/// Temperature register pointer. POR value selects this register, so a
/// plain two-byte read without a preceding pointer write also works if the
/// pointer register has not been changed.
const REG_TEMPERATURE: u8 = 0x00;

/// Datasheet supported range. The TMP102 measures −40..125 °C; the accepted
/// window adds 15 °C below and 25 °C above to catch bus glitches that decode
/// to plausible but impossible temperatures without rejecting valid
/// near-limit readings.
const MIN_MILLICELSIUS: i32 = -55_000;
const MAX_MILLICELSIUS: i32 = 150_000;

/// LSB is 0.0625 °C = 62.5 m°C, encoded as 125/2 to stay in integer math.
const MILLICELSIUS_NUMERATOR: i32 = 125;
const MILLICELSIUS_DENOMINATOR: i32 = 2;

/// One TMP102 on the I2C bus. Holds the address only; the bus is passed
/// in on each read so multiple sensors can share a `&mut I` without
/// each owning a copy.
pub struct Tmp102 {
    address: u8,
}

impl Tmp102 {
    /// Construct a driver for the TMP102 at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }

    /// Return the configured I2C address.
    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Read the temperature register and return millicelsius.
    ///
    /// Returns `Err` on a bus error or when the decoded value is outside
    /// the datasheet operating range.
    pub async fn read_millicelsius<I, E>(&self, i2c: &mut I) -> Result<i32, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let mut buffer = [0u8; 2];
        i2c.write_read(self.address, &[REG_TEMPERATURE], &mut buffer)
            .await
            .map_err(Error::I2c)?;
        let millicelsius = raw_to_millicelsius(buffer);
        if !(MIN_MILLICELSIUS..=MAX_MILLICELSIUS).contains(&millicelsius) {
            return Err(Error::OutOfRange(millicelsius));
        }
        Ok(millicelsius)
    }
}

/// Error returned by [`Tmp102::read_millicelsius`].
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// I2C bus error.
    I2c(E),
    /// Decoded value is outside the datasheet operating range.
    OutOfRange(i32),
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

#[cfg(test)]
#[path = "tests/tmp102.rs"]
mod tests;
