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

//! TMP464 four-channel remote-diode plus local I2C temperature sensor
//! driver.
//!
//! Each remote channel reads the die temperature of one attached ASIC; the
//! local channel reads the TMP464 package itself. The device powers on
//! with every channel enabled and converting continuously, so no
//! configuration write is needed for a read-only use.
//!
//! Registers are 16-bit, as on the LM75A and the TMP102, rather than the
//! TMP461's split 8-bit high/low pairs. The 13-bit two's-complement result
//! occupies bits 15:3, left-justified the same way TMP102 left-justifies
//! its 12-bit result; bits 2:0 are reserved and always read 0. Resolution
//! is a fixed 0.0625°C regardless of channel — there is no TMP461-style
//! extended-range mode to track. η-factor and offset calibration are
//! not applied.
//!
//! A remote channel open-circuit is reported by its own status bit rather
//! than by railing to a sentinel value, so each remote read costs two
//! transactions: the shared status register, then the channel's result
//! register.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::index::{ChipAddress, HashboardIndex};
use ubos_core::units::Temperature;

use crate::{Readings, TempSource, add_source};

#[cfg(test)]
#[path = "tests/tmp464.rs"]
mod tests;

/// Local temperature result register (16-bit, 13-bit result left-justified).
const REG_LOCAL: u8 = 0x00;
/// Remote Channel Open Status register (read-only, 16-bit). Bits 8 to 11
/// report the open-circuit state of remote channels one to four.
const REG_REMOTE_OPEN_STATUS: u8 = 0x23;

/// Value the result registers return for a shorted remote diode
/// as −256 °C. The local channel shares the same ADC and rails
/// here too on an internal fault, though the part defines the rail
/// only for the diode-fault comparator.
const SHORT_CIRCUIT: Temperature = Temperature::from_millicelsius(-256_000);

/// Valid range for a remote channel. The part spans −55..150 °C for
/// a remote diode, but a powered hashboard never runs below 0 °C; 160 °C
/// gives headroom above the diode ceiling for a transient bus glitch
/// without rejecting a valid near-limit reading.
const REMOTE_MIN: Temperature = Temperature::from_millicelsius(0);
const REMOTE_MAX: Temperature = Temperature::from_millicelsius(160_000);

/// Valid range for the local channel. The part spans −40..125 °C,
/// but a powered hashboard never runs below 0 °C, so rejecting sub-zero
/// local readings catches sensor faults without masking any valid
/// operating condition.
const LOCAL_MIN: Temperature = Temperature::from_millicelsius(0);
const LOCAL_MAX: Temperature = Temperature::from_millicelsius(160_000);

/// The 13-bit two's-complement result occupies bits 15:3 of the 16-bit
/// register; bits 2:0 are reserved and always read 0.
const RESULT_SHIFT: u32 = 3;
/// Resolution is 0.0625 °C per LSB, 62.5 m°C, kept as 125/2 to
/// stay in integer math.
const MILLICELSIUS_NUMERATOR: i32 = 125;
const MILLICELSIUS_DENOMINATOR: i32 = 2;

/// One of the four independent remote-diode channels. Each has its own
/// result register and its own bit in the open-status register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoteChannel {
    One,
    Two,
    Three,
    Four,
}

impl RemoteChannel {
    /// Every remote channel, in register order.
    pub const ALL: [Self; 4] = [Self::One, Self::Two, Self::Three, Self::Four];

    /// This channel's temperature result register.
    const fn register(self) -> u8 {
        match self {
            Self::One => 0x01,
            Self::Two => 0x02,
            Self::Three => 0x03,
            Self::Four => 0x04,
        }
    }

    /// This channel's bit in the Remote Channel Open Status register.
    const fn open_status_bit(self) -> u16 {
        match self {
            Self::One => 1 << 8,
            Self::Two => 1 << 9,
            Self::Three => 1 << 10,
            Self::Four => 1 << 11,
        }
    }
}

/// One TMP464 on the I2C bus: its address and what each of its channels
/// measures. The bus is passed in on each read so multiple devices can
/// share a `&mut I`.
#[derive(Clone, Copy)]
pub struct Tmp464 {
    /// The sensor's 7-bit I2C address.
    pub(crate) address: SevenBitAddress,
    /// What the local channel, the sensor's own package, measures.
    pub(crate) local: TempSource,
    /// What each remote diode measures, in [`RemoteChannel::ALL`] order,
    /// or `None` for a channel the board leaves unwired. An unwired channel
    /// is not read, since its open diode would only report a fault.
    pub(crate) remote: [Option<TempSource>; 4],
}

/// The source of the die of `chip` on `chain`, or `None` for an unwired
/// channel. It maps one element, because a `const fn` cannot map an array.
const fn chip_source(chain: HashboardIndex, chip: Option<ChipAddress>) -> Option<TempSource> {
    match chip {
        Some(chip) => Some(TempSource::Chip { chain, chip }),
        None => None,
    }
}

impl Tmp464 {
    /// The sensor at `address` whose package measures the hashboard on
    /// `chain`, and whose remote diodes measure the dies of `chips` on
    /// that chain, in [`RemoteChannel::ALL`] order. `None` is a channel
    /// that the board leaves unwired. The source of its local readings
    /// carries the same address, so a board states it once.
    #[must_use]
    pub const fn hashboard(
        chain: u8,
        address: SevenBitAddress,
        chips: [Option<ChipAddress>; 4],
    ) -> Self {
        let chain = HashboardIndex(chain);
        let [first, second, third, fourth] = chips;
        Self {
            address,
            local: TempSource::Hashboard { chain, address },
            remote: [
                chip_source(chain, first),
                chip_source(chain, second),
                chip_source(chain, third),
                chip_source(chain, fourth),
            ],
        }
    }

    /// What the local channel measures.
    #[must_use]
    pub const fn local(&self) -> TempSource {
        self.local
    }

    /// What each remote diode measures, in [`RemoteChannel::ALL`] order.
    #[must_use]
    pub const fn remote(&self) -> [Option<TempSource>; 4] {
        self.remote
    }

    /// Read the local and every wired remote channel, each as what its
    /// field names, or as the fault that kept it from a reading.
    pub async fn read_temperatures<I: I2c>(&self, i2c: &mut I) -> Readings<I::Error> {
        let mut readings = Readings::new();
        readings.extend([add_source(
            self.local,
            self.read_local_temperature(i2c).await,
        )]);
        // TODO: Read the shared open-status register once here rather than
        // once per channel, which saves three transactions per sample.
        for (channel, source) in RemoteChannel::ALL.into_iter().zip(self.remote) {
            let Some(source) = source else { continue };
            let remote = self.read_remote_temperature(i2c, channel).await;
            readings.extend([add_source(source, remote)]);
        }
        readings
    }

    /// Read one remote-diode channel.
    ///
    /// The remote diode is intended to be an ASIC die; this is the
    /// authoritative thermal reading for die-temperature protection.
    ///
    /// Returns `Err` on a bus error, when the channel's open-status bit
    /// is set, or when the decoded value is outside the operating range.
    pub async fn read_remote_temperature<I, E>(
        &self,
        i2c: &mut I,
        channel: RemoteChannel,
    ) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        // The open-status bit flags an open/unconnected remote diode —
        // the expected state when the ASIC it senses is unpowered.
        let status =
            u16::from_be_bytes(read_word(i2c, self.address, REG_REMOTE_OPEN_STATUS).await?);
        if status & channel.open_status_bit() != 0 {
            return Err(Error::OpenCircuit);
        }
        let buffer = read_word(i2c, self.address, channel.register()).await?;
        let temperature: Temperature = RawTmp464(buffer).into();
        #[cfg(feature = "defmt")]
        ubos_logging::trace!(
            "TMP464 @{:x} {:?}: status={:x} -> {} °C",
            self.address,
            ubos_logging::Debug2Format(&channel),
            status,
            temperature,
        );
        // A shorted remote diode reads exactly −256 °C; this is a channel
        // fault rail, not a real die temperature.
        if temperature == SHORT_CIRCUIT {
            return Err(Error::ShortCircuit);
        }
        if !(REMOTE_MIN..=REMOTE_MAX).contains(&temperature) {
            return Err(Error::OutOfRange(temperature));
        }
        Ok(temperature)
    }

    /// Read the local (on-package) channel.
    ///
    /// Useful for ambient context alongside the remote die readings.
    ///
    /// Returns `Err` on a bus error or when the decoded value is outside
    /// the operating range.
    pub async fn read_local_temperature<I, E>(&self, i2c: &mut I) -> Result<Temperature, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let buffer = read_word(i2c, self.address, REG_LOCAL).await?;
        let temperature: Temperature = RawTmp464(buffer).into();
        // The local channel on a working sensor never rails to the
        // remote-diode short-circuit sentinel. Reaching it indicates a
        // defective TMP464 package.
        if temperature == SHORT_CIRCUIT {
            #[cfg(feature = "defmt")]
            ubos_logging::warn!(
                "TMP464 @{:x}: local channel at short-circuit rail — sensor defective",
                self.address,
            );
            return Err(Error::ShortCircuit);
        }
        if !(LOCAL_MIN..=LOCAL_MAX).contains(&temperature) {
            return Err(Error::OutOfRange(temperature));
        }
        Ok(temperature)
    }
}

/// Error returned by [`Tmp464`] read operations.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// I2C bus error.
    I2c(E),
    /// Remote diode open-circuit (the channel's open-status bit is set).
    /// Expected when the sensed ASIC is unpowered.
    OpenCircuit,
    /// Remote diode shorted (the channel reads −256 °C).
    ShortCircuit,
    /// Decoded value is outside the operating range.
    OutOfRange(Temperature),
}

/// Read a 16-bit register word from `address` at `register`.
async fn read_word<I, E>(i2c: &mut I, address: u8, register: u8) -> Result<[u8; 2], Error<E>>
where
    I: I2c<Error = E>,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(address, &[register], &mut buffer)
        .await
        .map_err(Error::I2c)?;
    Ok(buffer)
}

/// A 16-bit big-endian temperature register word as read from the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawTmp464(pub(crate) [u8; 2]);

/// The 13-bit two's-complement result is left-justified in bits 15:3;
/// the arithmetic right-shift by 3 sign-extends it while discarding the
/// three reserved padding bits. LSB = 0.0625 °C = 62.5 m°C; multiplying
/// by 125 and dividing by 2 stays in integer math with no float.
impl From<RawTmp464> for Temperature {
    fn from(raw: RawTmp464) -> Self {
        let result = i16::from_be_bytes(raw.0) >> RESULT_SHIFT;
        Self::from_millicelsius(
            i32::from(result) * MILLICELSIUS_NUMERATOR / MILLICELSIUS_DENOMINATOR,
        )
    }
}
