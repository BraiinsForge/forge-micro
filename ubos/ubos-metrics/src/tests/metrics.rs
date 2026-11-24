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

//! Coverage for the render of one series: a stored value leaves as one
//! exposition line in its base unit.

use ubos_core::units::{Temperature, Voltage};
use ubos_http::BufferWriter;

use super::{METRIC_LINE_BYTES, StoredValue, render_metric};

/// The line `render_metric` writes for a stored value under `key`, with
/// the clock offset given.
fn line(key: &str, stored: &StoredValue, ntp_offset: u64) -> heapless::String<METRIC_LINE_BYTES> {
    let mut buffer = [0; METRIC_LINE_BYTES];
    let mut writer = BufferWriter::new(&mut buffer);
    render_metric(&mut writer, key, stored, ntp_offset);
    let written = writer.len();
    let text = core::str::from_utf8(&buffer[..written]).expect("a line is text");
    text.try_into().expect("a line fits its budget")
}

/// Before the clock is set a line carries the key and the value alone,
/// so the scraper stamps it.
#[test]
fn a_stored_value_renders_as_one_line_in_its_base_unit() {
    let stored = StoredValue {
        value: Temperature::from_millicelsius(25_500).into(),
        timestamp_ms: 1_000,
    };
    assert_eq!(
        line("board_ambient_temperature_celsius", &stored, 0).as_str(),
        "board_ambient_temperature_celsius 25.500\n"
    );
}

/// Once the clock is set the line ends with the time of the reading in
/// Unix milliseconds.
#[test]
fn a_line_carries_the_time_of_the_reading_once_the_clock_is_set() {
    let stored = StoredValue {
        value: Voltage::from_mv(21_600).into(),
        timestamp_ms: 1_000,
    };
    assert_eq!(
        line("hashboard_chain_voltage_volts{chain=\"0\"}", &stored, 2).as_str(),
        "hashboard_chain_voltage_volts{chain=\"0\"} 21.600 3000\n"
    );
}
