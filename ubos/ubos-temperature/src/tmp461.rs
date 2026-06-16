//! TMP461 (TMP451-compatible) dual-channel I2C temperature sensor driver.
//!
//! Reads the remote-diode channel to measure the die temperature of an
//! attached ASIC and the local channel for on-package ambient. The device
//! powers on in continuous-conversion mode; the active measurement range is
//! read from the Configuration register on each sample so the high-byte
//! decode matches the chip's mode.
//!
//! The high byte decodes per the TMP451/461 spec: 2's-complement degrees in
//! standard range, or unsigned with a −64 °C offset in extended range. The
//! low byte's upper nibble carries the 1/16 °C fraction (always positive).
//! Ideality correction and offset calibration are not applied.

use embedded_hal_async::i2c::I2c;

/// Status register (read-only). Reports ADC state and the remote-diode
/// connection fault.
const REG_STATUS: u8 = 0x02;
/// `OPEN` bit in the status register: 1 = remote diode open-circuit.
const STATUS_OPEN: u8 = 1 << 2;
/// Configuration register. Bit 2 (`RANGE`) selects extended range.
const REG_CONFIG: u8 = 0x03;
/// `RANGE` bit: 1 = extended (−64..191 °C, +64 offset), 0 = standard.
const CONFIG_EXTENDED_RANGE: u8 = 1 << 2;
/// Value the remote channel returns for a shorted diode (datasheet).
const SHORT_CIRCUIT_MILLICELSIUS: i32 = -64_000;
/// Full-scale standard-range rail (high=0x7F, low=0xF0): 127.9375 °C. An
/// unbiased or floating remote diode rails here; it is the high-side
/// counterpart to the −64 °C short rail and is not a trustworthy die
/// reading.
const SATURATED_MILLICELSIUS: i32 = 127_937;
/// Full-scale extended-range rail (high=0xFF, low=0xF0): 191.9375 °C.
/// Same meaning as `SATURATED_MILLICELSIUS` but for the extended range
/// where a floating diode reaches 0xFF rather than 0x7F.
const SATURATED_EXTENDED_MILLICELSIUS: i32 = 191_937;
/// Remote temperature high byte.
const REG_REMOTE_HIGH: u8 = 0x01;
/// Remote temperature low byte: bits 7:4 are the 1/16 °C fraction.
const REG_REMOTE_LOW: u8 = 0x10;
/// Local temperature high byte.
const REG_LOCAL_HIGH: u8 = 0x00;
/// Local temperature low byte: bits 7:4 are the 1/16 °C fraction.
const REG_LOCAL_LOW: u8 = 0x15;

/// Valid range for the remote channel in standard mode. The datasheet
/// reports 0..127 °C; 160 °C gives headroom for transient bus glitches
/// without rejecting valid near-limit readings.
const REMOTE_MIN_MILLICELSIUS: i32 = 0;
const REMOTE_MAX_MILLICELSIUS: i32 = 160_000;

/// Valid range for the local channel in standard mode. The datasheet
/// specifies −40 °C as the device minimum, but a powered hashboard never
/// operates below 0 °C, so rejecting sub-zero local readings catches sensor
/// faults without masking any valid operating condition.
const LOCAL_MIN_MILLICELSIUS: i32 = 0;
const LOCAL_MAX_MILLICELSIUS: i32 = 160_000;

/// Degrees-Celsius offset applied to the unsigned high byte in extended range.
const EXTENDED_RANGE_OFFSET_C: i32 = 64;
/// Millicelsius per whole degree.
const MILLICELSIUS_PER_DEGREE: i32 = 1000;
/// The low byte's upper nibble (bits 7:4) carries the fractional part.
const FRACTION_SHIFT: u32 = 4;
/// Each 1/16 °C fraction step is 62.5 m°C, encoded as 625/10 to stay integer.
const FRACTION_STEP_NUMERATOR: i32 = 625;
const FRACTION_STEP_DENOMINATOR: i32 = 10;

/// One TMP461 on the I2C bus. Holds the address only; the bus is passed
/// in on each read so multiple devices can share a `&mut I`.
pub struct Tmp461 {
    address: u8,
}

impl Tmp461 {
    /// Construct a driver for the TMP461 at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }

    /// Return the configured I2C address.
    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Read the remote-diode channel and return millicelsius.
    ///
    /// The remote diode is intended to be the ASIC die; this is the
    /// authoritative thermal reading for die-temperature protection.
    ///
    /// Returns `Err` on a bus error or when the decoded value is outside
    /// the standard operating range.
    pub async fn read_remote_millicelsius<I, E>(&self, i2c: &mut I) -> Result<i32, Error<E>>
    where
        I: I2c<Error = E>,
    {
        // The OPEN status bit flags an open/unconnected remote diode — the
        // expected state when the ASIC it senses is unpowered.
        let status = read_byte(i2c, self.address, REG_STATUS).await?;
        if status & STATUS_OPEN != 0 {
            return Err(Error::OpenCircuit);
        }
        let extended = self.read_extended_range(i2c).await?;
        // Read high before low: reading the high byte latches the low byte.
        let high = read_byte(i2c, self.address, REG_REMOTE_HIGH).await?;
        let low = read_byte(i2c, self.address, REG_REMOTE_LOW).await?;
        let millicelsius = decode_millicelsius(high, low, extended);
        #[cfg(feature = "defmt")]
        ubos_logging::trace!(
            "TMP461 remote @{:x}: status={:x} ext={} hi={:x} lo={:x} -> {} mC",
            self.address,
            status,
            extended,
            high,
            low,
            millicelsius,
        );
        // A shorted remote diode reads exactly −64 °C; an unbiased/floating
        // one rails to full scale. Both are channel-fault rails, not real
        // die temperatures. The saturation rail depends on the active range:
        // standard tops at 127.9375 °C, extended at 191.9375 °C.
        if millicelsius == SHORT_CIRCUIT_MILLICELSIUS {
            return Err(Error::ShortCircuit);
        }
        let saturated_rail = if extended {
            SATURATED_EXTENDED_MILLICELSIUS
        } else {
            SATURATED_MILLICELSIUS
        };
        if millicelsius == saturated_rail {
            return Err(Error::Saturated);
        }
        if !(REMOTE_MIN_MILLICELSIUS..=REMOTE_MAX_MILLICELSIUS).contains(&millicelsius) {
            return Err(Error::OutOfRange(millicelsius));
        }
        Ok(millicelsius)
    }

    /// Read the Configuration register and return whether extended range is
    /// active.
    async fn read_extended_range<I, E>(&self, i2c: &mut I) -> Result<bool, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let config = read_byte(i2c, self.address, REG_CONFIG).await?;
        Ok(config & CONFIG_EXTENDED_RANGE != 0)
    }

    /// Read the local (on-package) channel and return millicelsius.
    ///
    /// Useful for ambient context alongside the remote die reading.
    ///
    /// Returns `Err` on a bus error or when the decoded value is outside
    /// the standard operating range.
    pub async fn read_local_millicelsius<I, E>(&self, i2c: &mut I) -> Result<i32, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let extended = self.read_extended_range(i2c).await?;
        let high = read_byte(i2c, self.address, REG_LOCAL_HIGH).await?;
        let low = read_byte(i2c, self.address, REG_LOCAL_LOW).await?;
        let millicelsius = decode_millicelsius(high, low, extended);
        // The local channel on a working sensor never rails to the remote-diode
        // sentinel values. Reaching them indicates a defective TMP461 package.
        // The saturation rail depends on the active range.
        if millicelsius == SHORT_CIRCUIT_MILLICELSIUS {
            #[cfg(feature = "defmt")]
            ubos_logging::warn!(
                "TMP461 @{:x}: local channel at short-circuit rail — sensor defective",
                self.address,
            );
            return Err(Error::ShortCircuit);
        }
        let saturated_rail = if extended {
            SATURATED_EXTENDED_MILLICELSIUS
        } else {
            SATURATED_MILLICELSIUS
        };
        if millicelsius == saturated_rail {
            #[cfg(feature = "defmt")]
            ubos_logging::warn!(
                "TMP461 @{:x}: local channel at saturation rail — sensor defective",
                self.address,
            );
            return Err(Error::Saturated);
        }
        if !(LOCAL_MIN_MILLICELSIUS..=LOCAL_MAX_MILLICELSIUS).contains(&millicelsius) {
            return Err(Error::OutOfRange(millicelsius));
        }
        Ok(millicelsius)
    }
}

/// Error returned by [`Tmp461`] read operations.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// I2C bus error.
    I2c(E),
    /// Remote diode open-circuit (the `OPEN` status bit is set). Expected
    /// when the sensed ASIC is unpowered.
    OpenCircuit,
    /// Remote diode shorted (the channel reads −64 °C).
    ShortCircuit,
    /// Remote channel railed to full scale (127.9375 °C) — an unbiased or
    /// floating diode, not a real die temperature.
    Saturated,
    /// Decoded value is outside the standard operating range.
    OutOfRange(i32),
}

/// Read a single register byte from `address` at `register`.
async fn read_byte<I, E>(i2c: &mut I, address: u8, register: u8) -> Result<u8, Error<E>>
where
    I: I2c<Error = E>,
{
    let mut buffer = [0u8; 1];
    i2c.write_read(address, &[register], &mut buffer)
        .await
        .map_err(Error::I2c)?;
    Ok(buffer[0])
}

/// Decode a high/low byte pair to millicelsius.
///
/// In standard range the high byte is 2's-complement degrees; in extended
/// range it is unsigned with a −64 °C offset. The low byte's upper nibble
/// (bits 7:4) is the fractional part in 1/16 °C steps (62.5 m°C each),
/// always added as a positive quantity. Using `625 / 10` for the fractional
/// step keeps the result in integer millicelsius with no floats.
pub(crate) fn decode_millicelsius(high: u8, low: u8, extended_range: bool) -> i32 {
    let degrees = if extended_range {
        i32::from(high) - EXTENDED_RANGE_OFFSET_C
    } else {
        // Standard range encodes the high byte as 2's-complement degrees;
        // reinterpret the bits rather than a value-changing cast.
        i32::from(i8::from_ne_bytes([high]))
    };
    degrees * MILLICELSIUS_PER_DEGREE
        + i32::from(low >> FRACTION_SHIFT) * FRACTION_STEP_NUMERATOR / FRACTION_STEP_DENOMINATOR
}

#[cfg(test)]
#[path = "tests/tmp461.rs"]
mod tests;
