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

use core::mem::offset_of;

use embedded_hal_async::i2c::ErrorKind;
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

use super::*;
use crate::i2c::I2cEeprom;

/// A version 1 record of the `FAHB0001` hashboard, as a board stores it.
/// A change to the layout, the version or the CRC fails here first.
const FAHB0001: &str = concat!(
    "4272a98aec0f864589514842000046414842303030310000000000000000ffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff1f01",
);

/// A version 1 record of the `FACB0001` control board, with the data of
/// the board in version 1 of its layout.
const FACB0001: &str = concat!(
    "4272a98aec0f8645895143420000464143423030303100000000000000000200",
    "00000001eb63070042726169696e7320466f7267650000000000000000000000",
    "0000000000000000466f72676520417865000000000000000000000000000000",
    "0000000000000000ffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe8bd",
);

/// The record that `hex` spells.
fn image(hex: &str) -> [u8; RECORD_LEN] {
    assert_eq!(hex.len(), 2 * RECORD_LEN, "the image fills a record");
    let mut raw = [0; RECORD_LEN];
    for (byte, hex) in raw.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let hex = core::str::from_utf8(hex).expect("the image is ASCII");
        *byte = u8::from_str_radix(hex, 16).expect("the image is hex");
    }
    raw
}

/// Recompute the CRC after a test changes a field it covers.
fn seal(raw: &mut [u8; RECORD_LEN]) {
    let (covered, crc) = raw.split_at_mut(offset_of!(RawRecord<LayoutV1>, layout.crc));
    crc.copy_from_slice(
        &Crc::<u16>::new(&CRC_16_IBM_3740)
            .checksum(covered)
            .to_le_bytes(),
    );
}

/// `raw` with the `hw_type` field set to `text` and the CRC recomputed.
fn with_hw_type(mut raw: [u8; RECORD_LEN], text: &[u8]) -> [u8; RECORD_LEN] {
    let at = offset_of!(RawRecord<LayoutV1>, layout.hw_type);
    raw[at..at + HW_TYPE_LEN].fill(0);
    raw[at..at + text.len()].copy_from_slice(text);
    seal(&mut raw);
    raw
}

#[test]
fn a_hashboard_record_parses() {
    let raw = image(FAHB0001);
    let record = Record::parse(&raw).expect("the record parses");
    assert_eq!(
        (record.hw_type, record.hw_version, record.check_erased()),
        (HardwareType::Hashboard, "FAHB0001", Ok(()))
    );
}

#[test]
fn data_in_the_reserved_bytes_fails_the_erased_check() {
    let mut raw = image(FAHB0001);
    raw[offset_of!(RawRecord<LayoutV1>, layout.reserved)] = 0;
    seal(&mut raw);
    let record = Record::parse(&raw).expect("the record parses");
    assert_eq!(record.check_erased(), Err(Error::Reserved));
}

#[test]
fn a_record_reads_from_the_eeprom_at_its_address() {
    let mut i2c = Mock::new(&[Transaction::write_read(
        0x57,
        vec![0],
        image(FAHB0001).to_vec(),
    )]);
    let mut raw = [0; RECORD_LEN];
    let record = embassy_futures::block_on(I2cEeprom::new(0x57, &mut i2c).read_record(&mut raw));
    assert_eq!(record.map(|record| record.hw_version), Ok("FAHB0001"));
    i2c.done();
}

#[test]
fn an_eeprom_that_does_not_answer_fails_the_read() {
    let mut i2c =
        Mock::new(
            &[Transaction::write_read(0x57, vec![0], vec![0; RECORD_LEN])
                .with_error(ErrorKind::Other)],
        );
    let mut raw = [0; RECORD_LEN];
    let record = embassy_futures::block_on(I2cEeprom::new(0x57, &mut i2c).read_record(&mut raw));
    assert_eq!(record, Err(ReadError::I2c(ErrorKind::Other)));
    i2c.done();
}

#[test]
fn version_1_control_board_data_parses() {
    let raw = image(FACB0001);
    let record = Record::parse(&raw).expect("the record parses");
    assert_eq!(
        ControlBoardEepromLayout::V1.parse(&record),
        Ok(ControlBoardEeprom {
            mac_address: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            brand_color: [235, 99, 7],
            brand_name: Some("Braiins Forge"),
            device_name: Some("Forge Axe"),
        })
    );
}

#[test]
fn a_name_that_is_empty_or_not_ascii_reads_as_none() {
    let mut raw = image(FACB0001);
    let reserved = offset_of!(RawRecord<LayoutV1>, layout.reserved);
    raw[reserved + offset_of!(ControlBoardEepromV1, brand_name)] = ERASED;
    raw[reserved + offset_of!(ControlBoardEepromV1, device_name)] = 0;
    seal(&mut raw);
    let record = Record::parse(&raw).expect("the record parses");
    let data = ControlBoardEepromLayout::V1
        .parse(&record)
        .expect("the data parses");
    assert_eq!((data.brand_name, data.device_name), (None, None));
}

#[test]
fn data_after_the_control_board_data_is_rejected() {
    let mut raw = image(FACB0001);
    raw[offset_of!(RawRecord<LayoutV1>, layout.reserved) + size_of::<ControlBoardEepromV1>()] = 0;
    seal(&mut raw);
    let record = Record::parse(&raw).expect("the record parses");
    assert_eq!(
        ControlBoardEepromLayout::V1.parse(&record),
        Err(Error::Reserved)
    );
}

#[test]
fn hw_type_names_each_kind_of_board() {
    for (text, hw_type) in [
        (&b"HB"[..], HardwareType::Hashboard),
        (b"PSU", HardwareType::Psu),
        (b"CB", HardwareType::ControlBoard),
        (b"FP", HardwareType::FrontPanel),
    ] {
        let raw = with_hw_type(image(FAHB0001), text);
        assert_eq!(
            Record::parse(&raw).map(|record| record.hw_type),
            Ok(hw_type)
        );
    }
}

#[test]
fn an_unknown_hw_type_is_rejected() {
    let raw = with_hw_type(image(FAHB0001), b"XX");
    assert_eq!(Record::parse(&raw), Err(Error::HardwareType(*b"XX\0\0")));
}

#[test]
fn bad_magic_is_rejected() {
    let mut raw = image(FAHB0001);
    raw[0] = b'X';
    assert_eq!(Record::parse(&raw), Err(Error::Magic(*b"Xr")));
}

#[test]
fn another_layout_version_is_rejected() {
    let mut raw = image(FAHB0001);
    let at = offset_of!(RawRecord<LayoutV1>, eeprom_version);
    raw[at] ^= 1;
    seal(&mut raw);
    let mut version = LayoutV1::VERSION;
    version[0] ^= 1;
    assert_eq!(Record::parse(&raw), Err(Error::Version(version)));
}

#[test]
fn a_changed_byte_fails_the_crc() {
    let mut raw = image(FAHB0001);
    raw[offset_of!(RawRecord<LayoutV1>, layout.hw_version)] = b'X';
    assert_eq!(Record::parse(&raw), Err(Error::Crc));
}
