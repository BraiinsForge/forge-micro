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

use embassy_time::Duration;
use ubos_core::units::{Temperature, Voltage};
use ubos_http::BufferWriter;

use super::{render_metric, render_stored};
use crate::consts::Metrics;
use crate::freshness::Expiry;
use crate::metrics::METRIC_LINE_BYTES;
use crate::store::{MetricKey, StoredValue};

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
        expiry: Some(Expiry::new(1_000, Metrics::DEFAULT_TTL)),
    };
    assert_eq!(
        line("ambient_temperature_celsius", &stored, 0).as_str(),
        "ambient_temperature_celsius 25.500\n"
    );
}

/// Once the clock is set the line ends with the time of the reading in
/// Unix milliseconds.
#[test]
fn a_line_carries_the_time_of_the_reading_once_the_clock_is_set() {
    let stored = StoredValue {
        value: Voltage::from_mv(21_600).into(),
        expiry: Some(Expiry::new(1_000, Metrics::DEFAULT_TTL)),
    };
    assert_eq!(
        line("chip_voltage_sum_volts{hashboard=\"0\"}", &stored, 2).as_str(),
        "chip_voltage_sum_volts{hashboard=\"0\"} 21.600 3000\n"
    );
}

/// A constant has no time of measurement, so its line carries no
/// timestamp when the clock is set.
#[test]
fn a_constant_renders_with_no_timestamp_once_the_clock_is_set() {
    let stored = StoredValue {
        value: Voltage::from_mv(21_600).into(),
        expiry: None,
    };
    assert_eq!(
        line("psu_voltage_volts{type=\"readback\"}", &stored, 2).as_str(),
        "psu_voltage_volts{type=\"readback\"} 21.600\n"
    );
}

/// A state carries no timestamp when the clock is set, so the scraper
/// stamps it.
#[test]
fn a_state_renders_with_no_timestamp_once_the_clock_is_set() {
    let stored = StoredValue {
        value: Voltage::from_mv(21_600).into(),
        expiry: Some(Expiry::unstamped(1_000, Metrics::DEFAULT_TTL)),
    };
    assert_eq!(
        line("psu_voltage_volts{type=\"readback\"}", &stored, 2).as_str(),
        "psu_voltage_volts{type=\"readback\"} 21.600\n"
    );
}

const TTL_SECS: u32 = 10;
const TTL_MS: u64 = TTL_SECS as u64 * 1_000;
const TAKEN_AT_MS: u64 = 5_000;
const KEY: &str = "ambient_temperature_celsius";

/// The exposition of a map with one series, rendered at `now_ms`.
fn exposition(expiry: Option<Expiry>, ntp_offset: u64, now_ms: u64) -> heapless::String<128> {
    let mut map = heapless::index_map::FnvIndexMap::<MetricKey, StoredValue, 2>::new();
    let key = MetricKey(heapless::String::try_from(KEY).expect("the key fits"));
    let stored = StoredValue {
        value: Temperature::from_millicelsius(25_500).into(),
        expiry,
    };
    assert!(map.insert(key, stored).is_ok(), "the map has room");
    let mut buffer = [0; 128];
    let mut writer = BufferWriter::new(&mut buffer);
    render_stored(&mut writer, &map, ntp_offset, now_ms);
    let written = writer.len();
    let text = core::str::from_utf8(&buffer[..written]).expect("the body is text");
    text.try_into().expect("the body fits")
}

fn measured() -> Expiry {
    Expiry::new(TAKEN_AT_MS, Duration::from_secs(u64::from(TTL_SECS)))
}

/// The series is shown at an age equal to its TTL and hidden one
/// millisecond later.
#[test]
fn a_series_is_hidden_one_millisecond_past_its_ttl() {
    assert!(!exposition(Some(measured()), 0, TAKEN_AT_MS + TTL_MS).is_empty());
    assert!(exposition(Some(measured()), 0, TAKEN_AT_MS + TTL_MS + 1).is_empty());
}

/// The expiry reads the monotonic clock, so an unsynced clock hides a
/// series too.
#[test]
fn an_expired_series_is_hidden_before_the_clock_is_set() {
    assert!(exposition(Some(measured()), 0, TAKEN_AT_MS + TTL_MS + 1).is_empty());
}

/// A constant stays after a time far above any TTL, with no timestamp,
/// also when the clock is set.
#[test]
fn a_constant_stays_with_no_timestamp() {
    assert_eq!(
        exposition(None, 2, u64::MAX).as_str(),
        "ambient_temperature_celsius 25.500\n"
    );
}

/// A state expires by its TTL as a measurement does.
#[test]
fn a_state_is_hidden_one_millisecond_past_its_ttl() {
    let state = Expiry::unstamped(TAKEN_AT_MS, Duration::from_secs(u64::from(TTL_SECS)));
    assert!(!exposition(Some(state), 2, TAKEN_AT_MS + TTL_MS).is_empty());
    assert!(exposition(Some(state), 2, TAKEN_AT_MS + TTL_MS + 1).is_empty());
}

/// The ingest task can store a metric after the render read the clock.
#[test]
fn a_series_stored_after_the_clock_reading_is_shown() {
    assert!(!exposition(Some(measured()), 0, TAKEN_AT_MS - 1).is_empty());
}
