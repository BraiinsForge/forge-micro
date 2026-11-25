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

//! The record every EEPROM of a Forge machine holds: the kind of board it
//! sits on, which board of that kind, and the data of that board.
//!
//! A record fills the EEPROM. It is the magic, `eeprom_version`, and the
//! layout that the version names. `eeprom_version` is the schema hash of
//! the layout, the way `ubos-config` versions its schema, so a change to
//! the layout changes the version. Each layout ends with its own check, so
//! a later layout can check its bytes another way. A layout that master
//! holds does not change, because a board may store it already: a change
//! adds a layout and an arm to [`Record::parse`].

use core::mem::offset_of;

use crc::{CRC_16_IBM_3740, Crc};
use postcard_schema::Schema;
use postcard_schema::key::hash::fnv1a64::hash_ty_path;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, transmute_ref};

use crate::format::{ascii_field, ascii_text};

#[cfg(test)]
#[path = "tests/forge.rs"]
mod tests;

/// The length of a record, the size of the EEPROM, which a reader reads
/// whole.
pub const RECORD_LEN: usize = 256;

const MAGIC: [u8; 2] = *b"Br";
const VERSION_LEN: usize = 8;
/// The length of a layout, which fills the record after the version.
const LAYOUT_LEN: usize = RECORD_LEN - MAGIC.len() - VERSION_LEN;
const HW_TYPE_LEN: usize = 4;
const TEXT_LEN: usize = 16;
/// The value of an erased EEPROM byte, which a reserved byte keeps while
/// it holds no data.
const ERASED: u8 = 0xFF;

/// A record as stored. Every field is byte-aligned, so `#[repr(C)]` pins
/// the layout to declaration order with no padding.
#[repr(C)]
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
struct RawRecord<Layout> {
    magic: [u8; 2],
    eeprom_version: [u8; VERSION_LEN],
    layout: Layout,
}

/// Version 1 of the layout. Text fields are ASCII, padded with NUL.
#[repr(C)]
#[derive(FromBytes, KnownLayout, Immutable, Unaligned, Schema)]
struct LayoutV1 {
    /// The kind of board: `"HB"`, `"PSU"`, `"CB"` or `"FP"`.
    hw_type: [u8; HW_TYPE_LEN],
    /// Which board of its kind, the key of its descriptor.
    hw_version: [u8; TEXT_LEN],
    /// The data of the board, in the layout that its descriptor names,
    /// then erased bytes.
    reserved: [u8; 224],
    /// The CRC-16 of every byte before it, little-endian.
    crc: [u8; 2],
}

impl LayoutV1 {
    /// The schema hash of this layout. It is a literal, so the assertion
    /// below compares it with the schema rather than with itself.
    const VERSION: [u8; VERSION_LEN] = [0xA9, 0x8A, 0xEC, 0x0F, 0x86, 0x45, 0x89, 0x51];

    /// Parse a record of this layout, checking its CRC before its fields.
    fn parse(bytes: &[u8; RECORD_LEN]) -> Result<Record<'_>, Error> {
        let record: &RawRecord<Self> = transmute_ref!(bytes);
        let layout = &record.layout;
        check_crc_16(
            &bytes[..offset_of!(RawRecord<Self>, layout.crc)],
            layout.crc,
        )?;
        Ok(Record {
            hw_type: HardwareType::parse(layout.hw_type)?,
            hw_version: ascii_field(&layout.hw_version),
            reserved: &layout.reserved,
        })
    }
}

const _: () = assert!(
    u64::from_ne_bytes(version::<LayoutV1>()) == u64::from_ne_bytes(LayoutV1::VERSION),
    "The record layout is not version 1. A board may store version 1 already, so add a layout and an arm to Record::parse instead."
);

/// The schema hash of a layout, the `eeprom_version` of a record that
/// holds it. It covers the name and the type of each field, in order, and
/// every layout hashes under one path.
const fn version<Layout: Schema>() -> [u8; VERSION_LEN] {
    hash_ty_path::<Layout>("ubos_eeprom::forge::Layout")
}

/// Check `crc`, a little-endian CRC-16 (`CRC_16_IBM_3740`), against the
/// bytes it covers.
fn check_crc_16(covered: &[u8], crc: [u8; 2]) -> Result<(), Error> {
    if Crc::<u16>::new(&CRC_16_IBM_3740).checksum(covered) == u16::from_le_bytes(crc) {
        Ok(())
    } else {
        Err(Error::Crc)
    }
}

/// Check that every byte of `bytes` is erased, so that none holds data.
fn check_erased(bytes: &[u8]) -> Result<(), Error> {
    if bytes.iter().all(|byte| *byte == ERASED) {
        Ok(())
    } else {
        Err(Error::Reserved)
    }
}

/// The kind of board a record sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum HardwareType {
    Hashboard,
    Psu,
    ControlBoard,
    FrontPanel,
}

impl HardwareType {
    /// The kind of board that the `hw_type` text of a record names.
    fn parse(text: [u8; HW_TYPE_LEN]) -> Result<Self, Error> {
        match ascii_field(&text) {
            "HB" => Ok(Self::Hashboard),
            "PSU" => Ok(Self::Psu),
            "CB" => Ok(Self::ControlBoard),
            "FP" => Ok(Self::FrontPanel),
            _ => Err(Error::HardwareType(text)),
        }
    }
}

/// Failure parsing a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// The magic is not `"Br"`, so the EEPROM holds no record. It carries
    /// the bytes in place of the magic, which tell a blank EEPROM from one
    /// that holds something else.
    Magic([u8; 2]),
    /// `eeprom_version` names no layout that this build reads.
    Version([u8; VERSION_LEN]),
    /// The stored CRC does not match the computed one.
    Crc,
    /// `hw_type` names no kind of board.
    HardwareType([u8; HW_TYPE_LEN]),
    /// A reserved byte that holds no data of the board is not erased.
    Reserved,
}

/// Failure reading a record from an EEPROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ReadError<E> {
    /// The EEPROM does not answer on the bus.
    I2c(E),
    /// The EEPROM answers, but holds no record that this build reads.
    Record(Error),
}

/// A parsed record. A text field that is not ASCII reads as `"INVALID"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Record<'a> {
    /// The kind of board the record sits on.
    pub hw_type: HardwareType,
    /// Which board of its kind, the key of its descriptor.
    pub hw_version: &'a str,
    /// The data of the board, in the layout that its descriptor names,
    /// then erased bytes.
    pub reserved: &'a [u8],
}

impl<'a> Record<'a> {
    /// Parse a record: check its magic, pick its layout by
    /// `eeprom_version`, then parse that layout.
    ///
    /// # Errors
    ///
    /// Returns the [`Error`] of the first check that fails.
    pub fn parse(bytes: &'a [u8; RECORD_LEN]) -> Result<Self, Error> {
        let record: &RawRecord<[u8; LAYOUT_LEN]> = transmute_ref!(bytes);
        if record.magic != MAGIC {
            return Err(Error::Magic(record.magic));
        }
        match record.eeprom_version {
            LayoutV1::VERSION => LayoutV1::parse(bytes),
            version => Err(Error::Version(version)),
        }
    }

    /// Check that the reserved bytes hold no data, as they must when the
    /// descriptor of the board names no layout for them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Reserved`] when a reserved byte is not erased.
    pub fn check_erased(&self) -> Result<(), Error> {
        check_erased(self.reserved)
    }
}

/// A color as red, green and blue, one byte each.
pub type Rgb = [u8; 3];

/// The layouts of the data that a control-board record holds at the start
/// of its reserved bytes. The descriptor of a control board names the
/// layout that its EEPROM holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ControlBoardEepromLayout {
    V1,
}

impl ControlBoardEepromLayout {
    /// The control-board data that `record` holds in this layout. The
    /// reserved bytes after the data must be erased.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Reserved`] when the reserved bytes are too short
    /// for the data, or a byte after the data is not erased.
    pub fn parse<'a>(self, record: &Record<'a>) -> Result<ControlBoardEeprom<'a>, Error> {
        match self {
            Self::V1 => {
                let (data, after) = ControlBoardEepromV1::ref_from_prefix(record.reserved)
                    .map_err(|_| Error::Reserved)?;
                check_erased(after)?;
                let [red, green, blue, _] = data.brand_color;
                Ok(ControlBoardEeprom {
                    mac_address: data.mac_address,
                    brand_color: [red, green, blue],
                    brand_name: ascii_text(&data.brand_name).filter(|name| !name.is_empty()),
                    device_name: ascii_text(&data.device_name).filter(|name| !name.is_empty()),
                })
            }
        }
    }
}

/// Version 1 of the control-board data. Text fields are ASCII, padded
/// with NUL.
#[repr(C)]
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
struct ControlBoardEepromV1 {
    mac_address: [u8; 6],
    /// The brand color: red, green and blue, then a byte that pads it to
    /// four bytes.
    brand_color: [u8; 4],
    brand_name: [u8; 32],
    device_name: [u8; 32],
}

const _: () = assert!(
    size_of::<ControlBoardEepromV1>() <= offset_of!(LayoutV1, crc) - offset_of!(LayoutV1, reserved),
    "The control-board data does not fit the reserved bytes of record layout 1."
);

/// The data of a control board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ControlBoardEeprom<'a> {
    /// The MAC address of the board, from the range that its maker bought.
    pub mac_address: [u8; 6],
    pub brand_color: Rgb,
    /// The brand the machine ships under, or `None` when the field is
    /// empty or not ASCII.
    pub brand_name: Option<&'a str>,
    /// The name of the device, or `None` when the field is empty or not
    /// ASCII.
    pub device_name: Option<&'a str>,
}
