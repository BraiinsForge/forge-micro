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

use super::*;

// Every row of the part's temperature data format table, so the decode
// is checked against the vendor's own examples. Each row is the
// temperature in millicelsius, truncated toward zero as the decode's
// integer math is, beside the register's high and low byte.
#[test]
fn table_1_temperature_data_format() {
    let table_1 = [
        (-64_000, [0xE0, 0x00]),
        (-50_000, [0xE7, 0x00]),
        (-25_000, [0xF3, 0x80]),
        (-125, [0xFF, 0xF0]),
        (-62, [0xFF, 0xF8]),
        (0, [0x00, 0x00]),
        (62, [0x00, 0x08]),
        (125, [0x00, 0x10]),
        (187, [0x00, 0x18]),
        (250, [0x00, 0x20]),
        (312, [0x00, 0x28]),
        (375, [0x00, 0x30]),
        (437, [0x00, 0x38]),
        (500, [0x00, 0x40]),
        (562, [0x00, 0x48]),
        (625, [0x00, 0x50]),
        (687, [0x00, 0x58]),
        (750, [0x00, 0x60]),
        (812, [0x00, 0x68]),
        (875, [0x00, 0x70]),
        (937, [0x00, 0x78]),
        (1000, [0x00, 0x80]),
        (5000, [0x02, 0x80]),
        (10_000, [0x05, 0x00]),
        (25_000, [0x0C, 0x80]),
        (50_000, [0x19, 0x00]),
        (75_000, [0x25, 0x80]),
        (100_000, [0x32, 0x00]),
        (125_000, [0x3E, 0x80]),
        (127_000, [0x3F, 0x80]),
        (150_000, [0x4B, 0x00]),
        (175_000, [0x57, 0x80]),
        (191_000, [0x5F, 0x80]),
    ];
    for (millicelsius, register) in table_1 {
        assert_eq!(
            Temperature::from(RawTmp464(register)),
            Temperature::from_millicelsius(millicelsius),
            "{register:02X?}"
        );
    }
}
