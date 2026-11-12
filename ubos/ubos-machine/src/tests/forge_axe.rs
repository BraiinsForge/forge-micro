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

//! The lookups compose a Forge Axe of the boards that their records name,
//! the sensors of that machine fit `FORGE_AXE_MAX_SENSORS`, and a record
//! that is wrong in any way names no board.

use super::*;

/// Reserved bytes that hold no data, enough for the control-board data.
const ERASED: [u8; 128] = [0xFF; 128];

/// Version 1 of the control-board data: the MAC address, the brand color
/// with its pad byte, and the brand and device names padded with NUL.
const CONTROL_BOARD_DATA: &[u8; 74] = b"\x02\0\0\0\0\x01\xeb\x63\x07\0\
    Braiins Forge\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\
    Forge Axe\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// A record of a board of `hw_type`, named `hw_version`.
fn record<'a>(hw_type: HardwareType, hw_version: &'a str, reserved: &'a [u8]) -> Record<'a> {
    Record {
        hw_type,
        hw_version,
        reserved,
    }
}

/// The reserved bytes of a control board: its data, then erased bytes.
fn control_board_reserved() -> [u8; 128] {
    let mut reserved = ERASED;
    reserved[..CONTROL_BOARD_DATA.len()].copy_from_slice(CONTROL_BOARD_DATA);
    reserved
}

#[test]
fn the_boards_that_the_records_name_compose_the_machine() {
    let machine = MachineDescriptor::forge_axe(
        forge_axe_control_board(&record(
            HardwareType::ControlBoard,
            "FACB0001",
            &control_board_reserved(),
        ))
        .expect("the control board is known")
        .0,
        forge_axe_hashboard(&record(HardwareType::Hashboard, "FAHB0001", &ERASED))
            .expect("the hashboard is known"),
        forge_axe_psu(&record(HardwareType::Psu, "FAPS0001", &ERASED)).expect("the PSU is known"),
    );
    assert!(machine.sensor_count() <= FORGE_AXE_MAX_SENSORS);
    assert!(
        machine
            .sensors()
            .map(SensorDescriptor::address)
            .eq([0x48, 0x4A, 0x49])
    );
}

#[test]
fn the_control_board_record_gives_its_data() {
    let reserved = control_board_reserved();
    let (_, data) =
        forge_axe_control_board(&record(HardwareType::ControlBoard, "FACB0001", &reserved))
            .expect("the control board is known");
    assert_eq!(
        data.map(|data| (data.mac_address, data.brand_name)),
        Some(([0x02, 0x00, 0x00, 0x00, 0x00, 0x01], Some("Braiins Forge")))
    );
}

#[test]
fn a_record_of_another_kind_of_board_names_none() {
    assert_eq!(
        forge_axe_hashboard(&record(HardwareType::Psu, "FAPS0001", &ERASED)).err(),
        Some(BoardError::HardwareType(HardwareType::Psu))
    );
}

#[test]
fn a_record_of_an_unknown_board_names_none() {
    assert_eq!(
        forge_axe_hashboard(&record(HardwareType::Hashboard, "FAHB9999", &ERASED)).err(),
        Some(BoardError::HwVersion("FAHB9999"))
    );
}

#[test]
fn data_that_no_layout_names_fails_the_board() {
    assert_eq!(
        forge_axe_psu(&record(HardwareType::Psu, "FAPS0001", &[0; 64])).err(),
        Some(BoardError::Reserved(forge::Error::Reserved))
    );
}

#[test]
fn the_front_panel_record_names_the_front_panel() {
    assert!(forge_axe_front_panel(&record(HardwareType::FrontPanel, "FAFP0001", &ERASED)).is_ok());
}
