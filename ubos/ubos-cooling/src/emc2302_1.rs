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

//! The EMC2302-1 two-channel fan controller over I2C.
//!
//! The -1 variant corresponds to the specific [`ADDRESS`].
//!
//! The fans are [operated as an open-drain with normal polarity at the 26 kHz base frequency](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=14).
//! They are driven by PWM duty i.e. [Direct Setting mode](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=13),
//! which is chosen by [ENAGx = 0](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=38).
//! set in Fan Configuration registers on power-on-reset e.g.
//! [Fan Configuration 1 resets to 2Bh = 00101011b](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=25).
//!
//! There is a watchdog spawned on power-up which checks there is an activity on any of the fans,
//! if there is none for four seconds, it drives both fans to full drive,
//! so a board whose firmware never reaches the cooling loop still cools
//! ([4.11 Watchdog Timer, 4.11.1 Power-Up Operation](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=19)).
// TODO: Consider CONTINUOUS OPERATION.

use bitfield_struct::bitfield;
use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::units::{FanPwm, FanRpm};
use ubos_logging::{Debug2Format, info, warn};

use crate::{FanOutput, TachReader};

#[cfg(test)]
#[path = "tests/emc2302_1.rs"]
mod tests;

/// Number of fan channels, each with its own tachometer.
pub const FANS: usize = 2;

/// The EMC2302-1's I2C address (-2 uses `0x2F`).
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=21>
const ADDRESS: SevenBitAddress = 0x2E;

/// Fan Configuration 1 - ENAG[7],RNG[6:5],EDG[4:3],UDT[2:0]
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=38>
#[bitfield(u8)]
#[derive(PartialEq, Eq)]
struct FanConfig1 {
    /// UDT: update time of the closed-loop algorithm.
    #[bits(3)]
    update_time: u8,
    /// EDG: tach edges sampled per measurement.
    #[bits(2)]
    edges: u8,
    /// RNG: the tach range.
    #[bits(2)]
    range: Range,
    /// ENAG: closed-loop algorithm on; off is Direct Setting mode.
    closed_loop: bool,
}

/// RNG: the slowest speed the tach reads and the tach count multiplier m.
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=38>
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    /// m = 1.
    RpmMin500,
    /// m = 2.
    RpmMin1000,
    /// m = 4.
    RpmMin2000,
    /// m = 8.
    RpmMin4000,
}

impl Range {
    /// Fan Configuration 1 resets to 2Bh, RNG = 01
    /// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=25>
    const POWER_ON: Self = Self::RpmMin1000;

    /// The tach count multiplier m.
    const fn multiplier(self) -> u32 {
        match self {
            Self::RpmMin500 => 1,
            Self::RpmMin1000 => 2,
            Self::RpmMin2000 => 4,
            Self::RpmMin4000 => 8,
        }
    }

    /// RNG bits of each range.
    /// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=38>
    const RPM_MIN_500: u8 = 0b00;
    const RPM_MIN_1000: u8 = 0b01;
    const RPM_MIN_2000: u8 = 0b10;
    const RPM_MIN_4000: u8 = 0b11;

    const fn from_bits(bits: u8) -> Self {
        match bits {
            Self::RPM_MIN_500 => Self::RpmMin500,
            Self::RPM_MIN_1000 => Self::RpmMin1000,
            Self::RPM_MIN_2000 => Self::RpmMin2000,
            Self::RPM_MIN_4000 => Self::RpmMin4000,
            _ => unreachable!(),
        }
    }

    const fn into_bits(self) -> u8 {
        match self {
            Self::RpmMin500 => Self::RPM_MIN_500,
            Self::RpmMin1000 => Self::RPM_MIN_1000,
            Self::RpmMin2000 => Self::RPM_MIN_2000,
            Self::RpmMin4000 => Self::RPM_MIN_4000,
        }
    }
}

/// Factor to calculate RPM from TACH reading count and [`Range`] multiplier.
const RPM_FACTOR: u32 = 3_932_160;

/// [`RPM_FACTOR`] is calculated from these parameters.
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=16>
#[test]
fn test_rpm_factor_matches_parameters() {
    let poles = 2; // poles on the fan
    let n = 5; // edges measured, EDG = 01 at power-on
    let f_tach = 32_768; // tach clock in Hz
    let seconds = 60; // per minute
    assert_eq!((n - 1) * f_tach * seconds / poles, RPM_FACTOR);
}

/// A TACH Reading count with the [`Range`] multiplier it was measured at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawTach {
    count: u16,
    range: Range,
}

/// RPM of a TACH Reading count; 0 above the valid maximum and at zero.
/// Equation 4-3: RPM = [`RPM_FACTOR`] * m / COUNT.
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=17>
impl From<RawTach> for FanRpm {
    fn from(raw: RawTach) -> Self {
        match raw.count {
            count @ 1..=TACH_COUNT_VALID_MAX => {
                Self::from_rpm(RPM_FACTOR * raw.range.multiplier() / u32::from(count))
            }
            _ => Self::from_rpm(0),
        }
    }
}

/// TACH Reading, Registers 6-23 and 6-24 the count in the top 13 bits.
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=47>
#[bitfield(u16)]
struct TachReading {
    #[bits(3)]
    __: u8,
    #[bits(13)]
    count: u16,
}

/// [Valid TACH Count resets to F5h](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=25),
/// with [bit weights 4096 down to 32, i.e. count bits 12:5](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=43),
/// so F5h << 5 = 7840 counts, ~501 RPM * m. Any higher TACH Reading count
/// reads 0, as [above the Valid TACH Count a fan is stalled](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=17).
const TACH_COUNT_VALID_MAX: u16 = 0xF5 << 5;

/// One fan channel of the EMC2302: the label its RPM series and log lines
/// carry, and its two registers.
struct Channel {
    label: &'static str,
    /// Fan Drive Setting register. The PWM duty is the written value over 255.
    setting: u8,
    /// Fan Configuration 1 register.
    config: u8,
    /// TACH Reading high-byte register. A two-byte read returns the low
    /// byte next, latched when the high byte was read, so both bytes come
    /// from one measurement.
    tach: u8,
}

/// Fan `a` and fan `b` registers:
/// * [Fan Drive Setting Register](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=37)
/// * [Fan Configuration Registers](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=38)
/// * [TACH Reading Registers](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=47)
const CHANNELS: [Channel; FANS] = [
    Channel {
        label: "a",
        setting: 0x30,
        config: 0x32,
        tach: 0x3E,
    },
    Channel {
        label: "b",
        setting: 0x40,
        config: 0x42,
        tach: 0x4E,
    },
];

impl Channel {
    /// Set RNG to `range`, leaving Fan Configuration 1's other bits, and
    /// return the range in effect: `range`, the one read back if the write
    /// failed, or the power-on one if the read failed.
    async fn set_range<I: I2c>(&self, i2c: &mut I, range: Range) -> Range {
        let mut config = [0];
        if let Err(e) = i2c.write_read(ADDRESS, &[self.config], &mut config).await {
            warn!(
                "fan {}: config read failed: {:?}",
                self.label,
                Debug2Format(&e)
            );
            return Range::POWER_ON;
        }
        let [byte] = config;
        let old = FanConfig1::from_bits(byte);
        let new = old.with_range(range);
        if let Err(e) = i2c.write(ADDRESS, &[self.config, new.into_bits()]).await {
            warn!(
                "fan {}: config write failed: {:?}",
                self.label,
                Debug2Format(&e)
            );
            return old.range();
        }
        info!(
            "fan {}: config {:08b} -> {:08b}",
            self.label,
            old.into_bits(),
            new.into_bits()
        );
        range
    }

    async fn set_duty<I: I2c>(&self, i2c: &mut I, pwm: FanPwm) {
        let setting = duty_to_setting(pwm);
        if let Err(e) = i2c.write(ADDRESS, &[self.setting, setting]).await {
            warn!(
                "fan {}: setting write failed: {:?}",
                self.label,
                Debug2Format(&e)
            );
        }
    }

    async fn read_rpm<I: I2c>(&self, i2c: &mut I, range: Range) -> FanRpm {
        let mut reading = [0; 2];
        if let Err(e) = i2c.write_read(ADDRESS, &[self.tach], &mut reading).await {
            warn!(
                "fan {}: tach read failed: {:?}",
                self.label,
                Debug2Format(&e)
            );
            return FanRpm::from_rpm(0);
        }
        // TACH reading bytes are high then low
        // <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=25>
        let reading = u16::from_be_bytes(reading);
        RawTach {
            count: TachReading::from_bits(reading).count(),
            range,
        }
        .into()
    }
}

/// Scale a duty to the 8-bit Fan Setting, where `u8::MAX` is full drive.
/// [EQUATION 4-1: fan setting register value to drive (PWM duty)](https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=13)
fn duty_to_setting(pwm: FanPwm) -> u8 {
    let setting = u16::from(pwm.percent()) * u16::from(u8::MAX) / u16::from(FanPwm::FULL.percent());
    // A duty never exceeds full, so the setting never exceeds u8::MAX.
    u8::try_from(setting).unwrap_or(u8::MAX)
}

/// Writes the cooling loop's duty to both fans. Pass into the cooling task.
pub struct Fan<I>(I);

impl<I: I2c> FanOutput for Fan<I> {
    /// Write the duty to both fans on every call, changed or not, so a chip
    /// that reset since the last call gets its duty back within one call.
    async fn set_pwm(&mut self, pwm: FanPwm) {
        for channel in &CHANNELS {
            channel.set_duty(&mut self.0, pwm).await;
        }
    }
}

/// Reads both fans' tachometers into the cooling loop, each at the range
/// its channel holds.
pub struct Tach<I> {
    i2c: I,
    ranges: [Range; FANS],
}

/// Set `range` on both channels and hand out the fan and the tach, each
/// over its own handle from `i2c`. Free because it builds two peers, and
/// neither one is the receiver.
pub async fn new_with_range<I: I2c>(range: Range, mut i2c: impl FnMut() -> I) -> (Fan<I>, Tach<I>) {
    let mut bus = i2c();
    let ranges = [
        CHANNELS[0].set_range(&mut bus, range).await,
        CHANNELS[1].set_range(&mut bus, range).await,
    ];
    (Fan(bus), Tach { i2c: i2c(), ranges })
}

impl<I: I2c> TachReader<FANS> for Tach<I> {
    const LABELS: [Option<&'static str>; FANS] = [Some(CHANNELS[0].label), Some(CHANNELS[1].label)];

    async fn read_rpms(&mut self) -> [FanRpm; FANS] {
        [
            CHANNELS[0].read_rpm(&mut self.i2c, self.ranges[0]).await,
            CHANNELS[1].read_rpm(&mut self.i2c, self.ranges[1]).await,
        ]
    }
}
