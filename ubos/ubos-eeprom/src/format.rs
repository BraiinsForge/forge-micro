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

//! Braiins hashboard EEPROM wire format, byte-for-byte compatible with
//! the factory layout, so a hashboard already factory-programmed
//! parses unchanged here. The board section is followed by the
//! firmware section, each with its own magic and CRC-16/CCITT.
//!
//! The parsed `&str` fields borrow out of the raw buffer, which the
//! caller already owns for as long as it needs the parsed data.

use core::mem::size_of;

use crc::{CRC_16_IBM_3740, Crc};
use ubos_core::units::{Frequency, Power, Temperature, Voltage};
use zerocopy::byteorder::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[cfg(test)]
#[path = "tests/format.rs"]
mod tests;

pub const EEPROM_LEN: usize = BOARD_INFO_LEN + FIRMWARE_INFO_LEN;

const BOARD_INFO_LEN: usize = 192;
const FIRMWARE_INFO_LEN: usize = 64;
const MAGIC: [u8; 2] = *b"Br";
const VERSION_0: u8 = 0x00;
const MILLIVOLTS_PER_CENTIVOLT: u32 = 10;
const MILLIWATTS_PER_WATT: u32 = 1000;

/// Wire layout of `BoardInfoSection`. Every field has alignment 1, so
/// `#[repr(C)]` alone pins the layout to declaration order with no
/// padding.
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
struct RawBoardInfo {
    magic: [u8; 2],
    version: u8,
    product_name: [u8; 16],
    product_version: [u8; 16],
    product_batch: [u8; 16],
    serial_number: [u8; 16],
    asic_type: [u8; 10],
    asic_bin: [u8; 10],
    asic_count: u8,
    domains_count: u8,
    temperature_sensors_count: u8,
    voltage_centivolts: U16,
    frequency_mhz: U16,
    hashrate_gh: U32,
    power_w: U16,
    temperature_c: u8,
    reserved: [u8; 89],
    crc: U16,
}

const _: () = assert!(size_of::<RawBoardInfo>() == BOARD_INFO_LEN);

/// Wire layout of `FirmwareInfoSection`. See [`RawBoardInfo`].
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
struct RawFirmwareInfo {
    magic: [u8; 2],
    version: [u8; 8],
    compilation_date: [u8; 11],
    device_id: [u8; 3],
    serial_number: [u8; 10],
    reserved: [u8; 28],
    crc: U16,
}

const _: () = assert!(size_of::<RawFirmwareInfo>() == FIRMWARE_INFO_LEN);

/// The voltage field as stored, in centivolts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawCentivolts(u16);

impl From<RawCentivolts> for Voltage {
    fn from(raw: RawCentivolts) -> Self {
        Self::from_mv(u32::from(raw.0) * MILLIVOLTS_PER_CENTIVOLT)
    }
}

/// The frequency field as stored, in megahertz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawMegahertz(u16);

impl From<RawMegahertz> for Frequency {
    fn from(raw: RawMegahertz) -> Self {
        Self::from_mhz(u32::from(raw.0))
    }
}

/// The power field as stored, in whole watts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawWatts(u16);

impl From<RawWatts> for Power {
    fn from(raw: RawWatts) -> Self {
        Self::from_mw(u32::from(raw.0) * MILLIWATTS_PER_WATT)
    }
}

/// The temperature field as stored, in whole degrees Celsius.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawCelsius(u8);

impl From<RawCelsius> for Temperature {
    fn from(raw: RawCelsius) -> Self {
        Self::from_celsius(i32::from(raw.0))
    }
}

/// Failure parsing a raw EEPROM dump into [`HashboardEepromData`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// `BoardInfoSection`'s magic was not `"Br"`.
    BoardMagic,
    /// `FirmwareInfoSection`'s magic was not `"Br"`.
    FirmwareMagic,
    /// `version` was not the only structure version this crate
    /// understands.
    UnsupportedVersion(u8),
    /// `BoardInfoSection`'s stored CRC did not match the computed one.
    BoardCrc,
    /// `FirmwareInfoSection`'s stored CRC did not match the computed one.
    FirmwareCrc,
    /// A reserved padding region held something other than the
    /// erased-EEPROM filler byte (`0xFF`).
    ReservedBytes,
}

/// Parsed `BoardInfoSection`: product identity, ASIC info, and the
/// factory default operating point.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct BoardInfoSection<'a> {
    pub product_name: &'a str,
    pub product_version: &'a str,
    pub product_batch: &'a str,
    pub serial_number: &'a str,
    pub asic_type: &'a str,
    pub asic_bin: &'a str,
    pub asic_count: u8,
    pub domains_count: u8,
    pub temperature_sensors_count: u8,
    pub voltage: Voltage,
    pub frequency: Frequency,
    pub hashrate_gh: u32,
    pub power: Power,
    pub temperature: Temperature,
}

/// Parsed `FirmwareInfoSection`: the board MCU's own firmware identity.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FirmwareInfoSection<'a> {
    pub version: &'a str,
    pub compilation_date: &'a str,
    pub device_id: [u8; 3],
    pub serial_number: [u8; 10],
}

/// A fully validated hashboard EEPROM: both sections, magic/version/CRC
/// checked, reserved padding confirmed erased.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct HashboardEepromData<'a> {
    pub board_info: BoardInfoSection<'a>,
    pub firmware_info: FirmwareInfoSection<'a>,
}

impl<'a> HashboardEepromData<'a> {
    /// Parse and fully validate a raw EEPROM dump. The checks run in
    /// the factory tooling's order, so a captured dump either parses or
    /// fails the same way in both.
    ///
    /// # Errors
    ///
    /// Returns the [`Error`] variant of whichever check fails first.
    ///
    /// # Panics
    ///
    /// Never. Both raw structures are size-checked at compile time and
    /// `raw` is split into slices of exactly those lengths.
    pub fn parse(raw: &'a [u8; EEPROM_LEN]) -> Result<Self, Error> {
        let (board_bytes, firmware_bytes) = raw.split_at(BOARD_INFO_LEN);
        let board = RawBoardInfo::ref_from_bytes(board_bytes).expect("BUG: size checked above");
        let firmware =
            RawFirmwareInfo::ref_from_bytes(firmware_bytes).expect("BUG: size checked above");

        if board.magic != MAGIC {
            return Err(Error::BoardMagic);
        }
        if firmware.magic != MAGIC {
            return Err(Error::FirmwareMagic);
        }
        if board.version != VERSION_0 {
            return Err(Error::UnsupportedVersion(board.version));
        }

        let crc = Crc::<u16>::new(&CRC_16_IBM_3740);
        if crc.checksum(&board_bytes[..BOARD_INFO_LEN - 2]) != board.crc.get() {
            return Err(Error::BoardCrc);
        }
        if crc.checksum(&firmware_bytes[..FIRMWARE_INFO_LEN - 2]) != firmware.crc.get() {
            return Err(Error::FirmwareCrc);
        }

        if !board.reserved.iter().all(|b| *b == 0xFF)
            || !firmware.reserved.iter().all(|b| *b == 0xFF)
        {
            return Err(Error::ReservedBytes);
        }

        Ok(Self {
            board_info: BoardInfoSection {
                product_name: ascii_field(&board.product_name),
                product_version: ascii_field(&board.product_version),
                product_batch: ascii_field(&board.product_batch),
                serial_number: ascii_field(&board.serial_number),
                asic_type: ascii_field(&board.asic_type),
                asic_bin: ascii_field(&board.asic_bin),
                asic_count: board.asic_count,
                domains_count: board.domains_count,
                temperature_sensors_count: board.temperature_sensors_count,
                voltage: RawCentivolts(board.voltage_centivolts.get()).into(),
                frequency: RawMegahertz(board.frequency_mhz.get()).into(),
                hashrate_gh: board.hashrate_gh.get(),
                power: RawWatts(board.power_w.get()).into(),
                temperature: RawCelsius(board.temperature_c).into(),
            },
            firmware_info: FirmwareInfoSection {
                version: ascii_field(&firmware.version),
                compilation_date: ascii_field(&firmware.compilation_date),
                device_id: firmware.device_id,
                serial_number: firmware.serial_number,
            },
        })
    }
}

/// A field runs to its first NUL, or to its end where the factory padded
/// with `0xFF` and left no terminator, which real hardware has produced.
///
/// Anything that is not ASCII falls back to `"INVALID"`, as the factory
/// tooling does, so one garbled field does not void a record whose
/// magic, version and CRC all checked out.
pub(crate) fn ascii_field(bytes: &[u8]) -> &str {
    ascii_text(bytes).unwrap_or("INVALID")
}

/// The text of a NUL-terminated field before its first NUL, or the whole
/// field when no terminator is present, or `None` when it is not ASCII.
pub(crate) fn ascii_text(bytes: &[u8]) -> Option<&str> {
    let text = match bytes.iter().position(|b| *b == 0) {
        Some(nul) => &bytes[..nul],
        None => bytes,
    };
    core::str::from_utf8(text).ok().filter(|s| s.is_ascii())
}
