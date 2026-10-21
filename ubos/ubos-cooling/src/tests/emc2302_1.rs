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

//! The EMC2302-1 conversions: the datasheet's own example, the valid
//! count limit, a stopped fan, the ranges and the duty range.

use super::*;

/// Section 4.3: "3000 RPM, then the user would input the hexadecimal
/// equivalent of 1296 (51h in the TACH Target Register)".
/// <https://ww1.microchip.com/downloads/aemDocuments/documents/MSLD/ProductDocuments/DataSheets/EMC2301-2-3-5-Data-Sheet-DS20006532A.pdf#page=14>
#[test]
fn datasheet_example() {
    // 1296 is 510h; 51h is its leading two hex digits.
    assert_eq!(1296, 0x510);
    // At RNG = 00 (m = 1) the TACH Reading holds 1296 as 28h 80h.
    let reading = TachReading::new()
        .with_count(1296)
        .into_bits()
        .to_be_bytes();
    assert_eq!(reading, [0x28, 0x80]);
    // 51h in the high byte alone is 2592 counts, the power-on m = 2 count.
    let high_byte_only = TachReading::from_bits(u16::from_be_bytes([0x51, 0x00]));
    assert_eq!(high_byte_only.count(), 2 * 1296);
    // 1296 reads back 3034 RPM; 3000 RPM is 1310.72 counts.
    let count = TachReading::from_bits(u16::from_be_bytes(reading)).count();
    assert_eq!(count_to_rpm(count, Range::RpmMin500).rpm(), 3034);
}

/// The valid maximum converts; above it and zero read 0.
#[test]
fn count_past_the_valid_maximum_reads_stopped() {
    assert_eq!(
        count_to_rpm(TACH_COUNT_VALID_MAX, Range::RpmMin500).rpm(),
        501
    );
    for count in [TACH_COUNT_VALID_MAX + 1, 0x1FFF, 0] {
        assert_eq!(
            count_to_rpm(count, Range::RpmMin500).rpm(),
            0,
            "count {count}"
        );
    }
}

/// Register 6-13: RNG 00, 01, 10 and 11 multiply the count by 1, 2, 4, 8.
#[test]
fn range_multipliers() {
    let ranges = [
        (Range::RpmMin500, 1),
        (Range::RpmMin1000, 2),
        (Range::RpmMin2000, 4),
        (Range::RpmMin4000, 8),
    ];
    for (range, m) in ranges {
        assert_eq!(range.multiplier(), m, "{range:?}");
    }
}

/// Fan Configuration 1 resets to 2Bh: UDT 011, EDG 01, the power-on
/// range and ENAG off. Setting a range changes RNG only.
#[test]
fn range_in_fan_configuration_1() {
    let power_on = FanConfig1::from_bits(0x2B);
    assert_eq!(power_on.update_time(), 0b011);
    assert_eq!(power_on.edges(), 0b01);
    assert_eq!(power_on.range(), Range::POWER_ON);
    assert!(!power_on.closed_loop());
    assert_eq!(power_on.with_range(Range::RpmMin500).into_bits(), 0x0B);
    for range in [
        Range::RpmMin500,
        Range::RpmMin1000,
        Range::RpmMin2000,
        Range::RpmMin4000,
    ] {
        let config = power_on.with_range(range);
        assert_eq!(config.range(), range);
        assert_eq!(config.with_range(Range::POWER_ON), power_on, "{range:?}");
    }
}

#[test]
fn duty_scales_to_the_fan_setting() {
    assert_eq!(duty_to_setting(FanPwm::ZERO), 0);
    assert_eq!(duty_to_setting(FanPwm::from_percent(40)), 102);
    assert_eq!(duty_to_setting(FanPwm::FULL), u8::MAX);
}
