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

//! TMP461 (TMP451-compatible) dual-channel I2C temperature sensor driver.
//!
//! The remote-diode channel measures the die temperature of an attached
//! ASIC and the local channel the on-package ambient. The active
//! measurement range is read from the Configuration register on every
//! sample, because it decides how the high byte decodes.
//!
//! Ideality correction and offset calibration are not applied.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::index::{ChipAddress, HashboardIndex};
use ubos_core::units::Temperature;

use crate::{Readings, TempSource, add_source};

#[cfg(test)]
#[path = "tests/tmp461.rs"]
mod tests;

/// Reports the ADC state and the remote-diode connection fault.
const REG_STATUS: u8 = 0x02;
/// `OPEN` bit in the status register: 1 = remote diode open-circuit.
const STATUS_OPEN: u8 = 1 << 2;
/// Configuration register. Bit 2 (`RANGE`) selects extended range.
const REG_CONFIG: u8 = 0x03;
/// `RANGE` bit: 1 = extended (−64..191 °C, +64 offset), 0 = standard.
const CONFIG_EXTENDED_RANGE: u8 = 1 << 2;
/// Value the remote channel returns for a shorted diode.
const SHORT_CIRCUIT: Temperature = Temperature::from_millicelsius(-64_000);
/// Full-scale rail in standard range. An unbiased or floating remote diode
/// rails here, so the value is a channel fault and not a die reading.
const SATURATED: Temperature = Temperature::from_millicelsius(127_937);
/// The extended-range counterpart of `SATURATED`.
const SATURATED_EXTENDED: Temperature = Temperature::from_millicelsius(191_937);
const REG_REMOTE_HIGH: u8 = 0x01;
const REG_REMOTE_LOW: u8 = 0x10;
const REG_LOCAL_HIGH: u8 = 0x00;
const REG_LOCAL_LOW: u8 = 0x15;

/// Valid range for the remote channel in standard mode. The part reports
/// 0..127 °C, and the headroom above that keeps a valid near-limit reading
/// out of the out-of-range path.
const REMOTE_MIN: Temperature = Temperature::from_millicelsius(0);
const REMOTE_MAX: Temperature = Temperature::from_millicelsius(160_000);

/// Valid range for the local channel in standard mode. The part
/// specifies −40 °C as the device minimum, but a powered hashboard never
/// operates below 0 °C, so rejecting sub-zero local readings catches sensor
/// faults without masking any valid operating condition.
const LOCAL_MIN: Temperature = Temperature::from_millicelsius(0);
const LOCAL_MAX: Temperature = Temperature::from_millicelsius(160_000);

/// Degrees-Celsius offset applied to the unsigned high byte in extended range.
const EXTENDED_RANGE_OFFSET_C: i32 = 64;
const MILLICELSIUS_PER_DEGREE: i32 = 1000;
/// The low byte's upper nibble (bits 7:4) carries the fractional part.
const FRACTION_SHIFT: u32 = 4;
/// Each 1/16 °C fraction step is 62.5 m°C, encoded as 625/10 to stay integer.
const FRACTION_STEP_NUMERATOR: i32 = 625;
const FRACTION_STEP_DENOMINATOR: i32 = 10;

#[derive(Clone, Copy)]
pub struct Tmp461 {
    pub(crate) address: SevenBitAddress,
    /// What the local channel, the sensor's own package, measures.
    pub(crate) local: TempSource,
    /// What the remote diode measures.
    pub(crate) remote: TempSource,
}

impl Tmp461 {
    /// The sensor at `address` whose package measures the hashboard on
    /// `chain`, and whose remote diode measures the die of `chip` on that
    /// chain. The source of its local readings carries the same address,
    /// so a board states it once.
    #[must_use]
    pub const fn hashboard(chain: u8, address: SevenBitAddress, chip: ChipAddress) -> Self {
        let chain = HashboardIndex(chain);
        Self {
            address,
            local: TempSource::Hashboard { chain, address },
            remote: TempSource::Chip { chain, chip },
        }
    }

    /// What the local channel measures.
    #[must_use]
    pub const fn local(&self) -> TempSource {
        self.local
    }

    /// What the remote diode measures.
    #[must_use]
    pub const fn remote(&self) -> TempSource {
        self.remote
    }

    pub async fn read_temperatures<I: I2c>(&self, i2c: &mut I) -> Readings<I::Error> {
        let local = add_source(self.local, self.read_local_temperature(i2c).await);
        let remote = add_source(self.remote, self.read_remote_temperature(i2c).await);
        [local, remote].into_iter().collect()
    }

    /// The remote diode is intended to be the ASIC die, so this is the
    /// authoritative reading for die-temperature protection.
    pub async fn read_remote_temperature<I, E>(&self, i2c: &mut I) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let status = read_byte(i2c, self.address, REG_STATUS).await?;
        if status & STATUS_OPEN != 0 {
            return Err(Error::OpenCircuit);
        }
        let extended = self.read_extended_range(i2c).await?;
        // Read high before low: reading the high byte latches the low byte.
        let high = read_byte(i2c, self.address, REG_REMOTE_HIGH).await?;
        let low = read_byte(i2c, self.address, REG_REMOTE_LOW).await?;
        let temperature: Temperature = RawTmp461 {
            high,
            low,
            extended,
        }
        .into();
        #[cfg(feature = "defmt")]
        ubos_logging::trace!(
            "TMP461 remote @{:x}: status={:x} ext={} hi={:x} lo={:x} -> {} °C",
            self.address,
            status,
            extended,
            high,
            low,
            temperature,
        );
        // Both rails are channel faults rather than temperatures, and which
        // saturation rail applies depends on the active range.
        if temperature == SHORT_CIRCUIT {
            return Err(Error::ShortCircuit);
        }
        let saturated_rail = if extended {
            SATURATED_EXTENDED
        } else {
            SATURATED
        };
        if temperature == saturated_rail {
            return Err(Error::Saturated);
        }
        if !(REMOTE_MIN..=REMOTE_MAX).contains(&temperature) {
            return Err(Error::OutOfRange(temperature));
        }
        Ok(temperature)
    }

    async fn read_extended_range<I, E>(&self, i2c: &mut I) -> Result<bool, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let config = read_byte(i2c, self.address, REG_CONFIG).await?;
        Ok(config & CONFIG_EXTENDED_RANGE != 0)
    }

    pub async fn read_local_temperature<I, E>(&self, i2c: &mut I) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let extended = self.read_extended_range(i2c).await?;
        let high = read_byte(i2c, self.address, REG_LOCAL_HIGH).await?;
        let low = read_byte(i2c, self.address, REG_LOCAL_LOW).await?;
        let temperature: Temperature = RawTmp461 {
            high,
            low,
            extended,
        }
        .into();
        // A working sensor never rails the local channel to a remote-diode
        // sentinel, so reaching one means a defective package.
        if temperature == SHORT_CIRCUIT {
            #[cfg(feature = "defmt")]
            ubos_logging::warn!(
                "TMP461 @{:x}: local channel at short-circuit rail — sensor defective",
                self.address,
            );
            return Err(Error::ShortCircuit);
        }
        let saturated_rail = if extended {
            SATURATED_EXTENDED
        } else {
            SATURATED
        };
        if temperature == saturated_rail {
            #[cfg(feature = "defmt")]
            ubos_logging::warn!(
                "TMP461 @{:x}: local channel at saturation rail — sensor defective",
                self.address,
            );
            return Err(Error::Saturated);
        }
        if !(LOCAL_MIN..=LOCAL_MAX).contains(&temperature) {
            return Err(Error::OutOfRange(temperature));
        }
        Ok(temperature)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    I2c(E),
    /// Remote diode open-circuit, which is the expected state while the
    /// sensed ASIC is unpowered.
    OpenCircuit,
    /// Remote diode shorted, reported as the `SHORT_CIRCUIT` rail.
    ShortCircuit,
    /// Channel railed to full scale, which an unbiased or floating diode
    /// does.
    Saturated,
    OutOfRange(Temperature),
}

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

/// A temperature as the chip holds it: the high and low register bytes
/// and the range mode that gives the high byte its meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawTmp461 {
    pub(crate) high: u8,
    pub(crate) low: u8,
    pub(crate) extended: bool,
}

/// In standard range the high byte is 2's-complement degrees; in extended
/// range it is unsigned with a −64 °C offset. The low byte's fraction is
/// always added as a positive quantity.
impl From<RawTmp461> for Temperature {
    fn from(raw: RawTmp461) -> Self {
        let degrees = if raw.extended {
            i32::from(raw.high) - EXTENDED_RANGE_OFFSET_C
        } else {
            // Reinterpret the bits rather than take a value-changing cast.
            i32::from(i8::from_ne_bytes([raw.high]))
        };
        Self::from_millicelsius(
            degrees * MILLICELSIUS_PER_DEGREE
                + i32::from(raw.low >> FRACTION_SHIFT) * FRACTION_STEP_NUMERATOR
                    / FRACTION_STEP_DENOMINATOR,
        )
    }
}
