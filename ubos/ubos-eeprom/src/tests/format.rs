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

use super::*;

/// Build a valid 256-byte EEPROM dump: `"BMM101 P00"` product name, a
/// four-chip BM1362 BMM101 hashboard's worth of fields, and both
/// sections' CRC-16/CCITT correctly filled in. Individual tests corrupt
/// one field of the result to exercise a specific validation failure.
fn valid_dump() -> [u8; EEPROM_LEN] {
    let mut board = RawBoardInfo {
        magic: MAGIC,
        version: VERSION_0,
        product_name: text(b"BMM101 P00"),
        product_version: text(b"1.0.0"),
        product_batch: text(b"20240101-001"),
        serial_number: text(b"SN0000001"),
        asic_type: text(b"BM1362AH"),
        asic_bin: text(b"B3"),
        asic_count: 4,
        domains_count: 4,
        temperature_sensors_count: 4,
        voltage_centivolts: U16::new(140),
        frequency_mhz: U16::new(525),
        hashrate_gh: U32::new(3_000),
        power_w: U16::new(100),
        temperature_c: 45,
        reserved: [0xFF; 89],
        crc: U16::new(0),
    };
    let mut firmware = RawFirmwareInfo {
        magic: MAGIC,
        version: text(b"1.0.0"),
        compilation_date: text(b"01.01.2024"),
        device_id: [0x01, 0x02, 0x03],
        serial_number: [0xAA; 10],
        reserved: [0xFF; 28],
        crc: U16::new(0),
    };

    let crc = Crc::<u16>::new(&CRC_16_IBM_3740);
    board.crc = U16::new(crc.checksum(&board.as_bytes()[..BOARD_INFO_LEN - 2]));
    firmware.crc = U16::new(crc.checksum(&firmware.as_bytes()[..FIRMWARE_INFO_LEN - 2]));

    let mut raw = [0u8; EEPROM_LEN];
    raw[..BOARD_INFO_LEN].copy_from_slice(board.as_bytes());
    raw[BOARD_INFO_LEN..].copy_from_slice(firmware.as_bytes());
    raw
}

/// Right-pad `text` with a NUL terminator, leaving the rest of the
/// field `0xFF` — matching the erased-EEPROM filler `bos-main` leaves
/// past a field's content.
fn text<const N: usize>(text: &[u8]) -> [u8; N] {
    let mut field = [0xFFu8; N];
    field[..text.len()].copy_from_slice(text);
    if text.len() < N {
        field[text.len()] = 0;
    }
    field
}

/// Recompute and store `BoardInfoSection`'s CRC over the current
/// contents of `raw[..BOARD_INFO_LEN]`, for tests that corrupt a board
/// field other than the CRC itself and need the CRC to still match.
fn refresh_board_crc(raw: &mut [u8; EEPROM_LEN]) {
    let crc = Crc::<u16>::new(&CRC_16_IBM_3740);
    let board_crc = crc.checksum(&raw[..BOARD_INFO_LEN - 2]);
    raw[BOARD_INFO_LEN - 2..BOARD_INFO_LEN].copy_from_slice(&board_crc.to_le_bytes());
}

#[test]
fn parses_a_valid_dump() {
    let raw = valid_dump();
    let data = HashboardEepromData::parse(&raw).expect("valid dump must parse");

    assert_eq!(data.board_info.product_name, "BMM101 P00");
    assert_eq!(data.board_info.asic_type, "BM1362AH");
    assert_eq!(data.board_info.asic_count, 4);
    assert_eq!(data.board_info.domains_count, 4);
    assert_eq!(data.board_info.voltage_centivolts, 140);
    assert_eq!(data.board_info.frequency_mhz, 525);
    assert_eq!(data.board_info.hashrate_gh, 3_000);
    assert_eq!(data.firmware_info.version, "1.0.0");
    assert_eq!(data.firmware_info.device_id, [0x01, 0x02, 0x03]);
}

#[test]
fn rejects_bad_board_magic() {
    let mut raw = valid_dump();
    raw[offset_of!(RawBoardInfo, magic)] = b'X';
    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::BoardMagic));
}

#[test]
fn rejects_bad_firmware_magic() {
    let mut raw = valid_dump();
    raw[BOARD_INFO_LEN + offset_of!(RawFirmwareInfo, magic)] = b'X';
    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::FirmwareMagic));
}

#[test]
fn rejects_erased_eeprom() {
    let raw = [0xFFu8; EEPROM_LEN];
    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::BoardMagic));
}

#[test]
fn rejects_zeroed_eeprom() {
    let raw = [0x00u8; EEPROM_LEN];
    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::BoardMagic));
}

#[test]
fn rejects_unsupported_version() {
    let mut raw = valid_dump();
    raw[offset_of!(RawBoardInfo, version)] = 0x01;
    assert_eq!(
        HashboardEepromData::parse(&raw),
        Err(Error::UnsupportedVersion(0x01))
    );
}

#[test]
fn rejects_bad_board_crc() {
    let mut raw = valid_dump();
    raw[offset_of!(RawBoardInfo, crc)] ^= 0xFF;
    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::BoardCrc));
}

#[test]
fn rejects_bad_firmware_crc() {
    let mut raw = valid_dump();
    raw[BOARD_INFO_LEN + offset_of!(RawFirmwareInfo, crc)] ^= 0xFF;
    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::FirmwareCrc));
}

#[test]
fn rejects_corrupted_reserved_bytes() {
    let mut raw = valid_dump();
    raw[offset_of!(RawBoardInfo, reserved)] = 0x00;
    refresh_board_crc(&mut raw);

    assert_eq!(HashboardEepromData::parse(&raw), Err(Error::ReservedBytes));
}

#[test]
fn non_ascii_text_field_falls_back_to_invalid_but_still_parses() {
    let mut raw = valid_dump();
    // 0xC3 alone is not valid UTF-8, and even if it were, is not ASCII.
    // A CRC-verified record with one garbled text field is not
    // hypothetical: real hardware has produced exactly this.
    raw[offset_of!(RawBoardInfo, product_name)] = 0xC3;
    refresh_board_crc(&mut raw);

    let data = HashboardEepromData::parse(&raw).expect("one bad text field must not fail parsing");
    assert_eq!(data.board_info.product_name, "INVALID");
    assert_eq!(data.board_info.asic_type, "BM1362AH");
}

#[test]
fn unterminated_field_uses_full_width() {
    let mut raw = valid_dump();
    // Fill the whole 10-byte asic_bin field with no NUL terminator.
    let asic_bin = offset_of!(RawBoardInfo, asic_bin);
    raw[asic_bin..asic_bin + 10].copy_from_slice(b"ABCDEFGHIJ");
    refresh_board_crc(&mut raw);

    let data = HashboardEepromData::parse(&raw).expect("valid dump must parse");
    assert_eq!(data.board_info.asic_bin, "ABCDEFGHIJ");
}
