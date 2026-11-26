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

//! Coverage for the key budget: every metric the tree renders must fit
//! [`super::KEY_CAPACITY`], which is not checked at compile time for a
//! metric the constant does not name. Coverage too for the slot a full
//! map gives to a new series.

extern crate std;

use std::format;

use embassy_time::{Duration, block_for};

use ubos_core::index::HashboardIndex;
use ubos_core::units::{Temperature, Voltage};

use super::{
    KEY_CAPACITY, LONGEST_KEY_LABELS, LONGEST_KEY_NAME, LONGEST_RENDERED_KEY_BYTES, METRICS,
    MetricKey, StoredValue, report_dropped, upsert_metric,
};
use crate::catalog::{CHIP_ADDR_LABEL, CHIP_TEMPERATURE, HASHBOARD_LABEL};
use crate::consts::Metrics;
use crate::metric;
use crate::metrics::{Metric, dropped};
use crate::name::Name;
use crate::value::{Count, Value};

/// A temperature for a test that reads the key of a metric and never its
/// value.
const ANY_TEMPERATURE: Temperature = Temperature::from_celsius(60);

// The length the capacity is asserted against must be the length of the
// key `from_metric` actually builds, or the compile-time check guards
// nothing.
#[test]
fn the_longest_key_constants_match_what_from_metric_renders() {
    let metric = metric!(LONGEST_KEY_NAME, ANY_TEMPERATURE)
        .label(HASHBOARD_LABEL, HashboardIndex(0))
        .label(CHIP_ADDR_LABEL, u8::MAX);
    let key = MetricKey::from_metric(&metric).expect("the longest key must encode");
    assert_eq!(
        key.0.as_str(),
        format!("{}{LONGEST_KEY_LABELS}", LONGEST_KEY_NAME.as_str())
    );
    assert_eq!(key.0.len(), LONGEST_RENDERED_KEY_BYTES);
}

// A push site adds its labels one call at a time, in any order, and the
// key holds them sorted by name with the text each value displays.
#[test]
fn labels_added_one_by_one_render_sorted_in_the_key() {
    let metric = metric!("chip_temperature_celsius", ANY_TEMPERATURE)
        .label(HASHBOARD_LABEL, HashboardIndex(0))
        .label(CHIP_ADDR_LABEL, 255_usize);
    let key = MetricKey::from_metric(&metric).expect("the key must encode");
    assert_eq!(
        key.0.as_str(),
        "chip_temperature_celsius{chip_addr=\"255\",hashboard=\"0\"}"
    );
}

// A key one byte past the cap is dropped rather than truncated: a
// truncated key would collide with another metric's.
#[test]
fn a_key_past_the_budget_is_refused() {
    let name = Name::<Count>::new(
        "this_name_is_deliberately_longer_than_the_key_capacity_allows_for_any_metric_at_all",
    );
    assert!(name.as_str().len() > KEY_CAPACITY);
    assert!(MetricKey::from_metric(&Metric::new(name, Count(0))).is_none());
}

type Map = heapless::index_map::FnvIndexMap<MetricKey, StoredValue, 2>;

fn upsert(map: &mut Map, name: &'static str, ttl: Duration) {
    upsert_metric(map, &Metric::new(Name::new(name), Count(0)).ttl(ttl));
}

fn holds(map: &Map, name: &str) -> bool {
    map.keys().any(|key| key.0.as_str() == name)
}

// A full map drops its expired series, so a stopped publisher does not
// keep a slot that a live series needs.
#[test]
fn a_full_map_gives_the_slot_of_an_expired_series_to_a_new_one() {
    let mut map = Map::new();
    upsert(&mut map, "stopped", Duration::from_secs(1));
    upsert(&mut map, "live", Duration::from_secs(3_600));
    block_for(Duration::from_millis(1_100));
    upsert(&mut map, "new", Duration::from_secs(3_600));
    assert!(holds(&map, "new") && holds(&map, "live") && !holds(&map, "stopped"));
}

// A full map of live series keeps them and drops the new one.
#[test]
fn a_full_map_of_live_series_refuses_a_new_one() {
    let mut map = Map::new();
    upsert(&mut map, "first", Duration::from_secs(3_600));
    upsert(&mut map, "second", Duration::from_secs(3_600));
    upsert(&mut map, "third", Duration::from_secs(3_600));
    assert!(holds(&map, "first") && holds(&map, "second") && !holds(&map, "third"));
}

// A chip series carries the hashboard and the slot of its chip, and its
// voltage renders as a key of its own.
#[test]
fn a_chip_voltage_keys_on_the_hashboard_and_the_chip() {
    let metric = metric!("chip_voltage_volts", Voltage::from_mv(1_200))
        .label(HASHBOARD_LABEL, HashboardIndex(1))
        .label(CHIP_ADDR_LABEL, 7_u8);
    let key = MetricKey::from_metric(&metric).expect("the key must encode");
    assert_eq!(
        key.0.as_str(),
        "chip_voltage_volts{chip_addr=\"7\",hashboard=\"1\"}"
    );
}

// A second reading of a chip replaces the first and carries a later
// time.
#[test]
fn a_second_reading_of_a_chip_replaces_the_first() {
    let mut map = heapless::index_map::FnvIndexMap::<MetricKey, StoredValue, 2>::new();
    let reading = |millivolts| {
        metric!("chip_voltage_volts", Voltage::from_mv(millivolts))
            .label(HASHBOARD_LABEL, HashboardIndex(0))
            .label(CHIP_ADDR_LABEL, 3_u8)
    };
    upsert_metric(&mut map, &reading(1_200));
    let first = map
        .values()
        .next()
        .and_then(|stored| stored.expiry)
        .expect("the first reading is stored");
    block_for(Duration::from_millis(5));
    upsert_metric(&mut map, &reading(1_250));
    let stored = map.values().next().expect("the reading is stored");
    assert_eq!(map.len(), 1);
    assert_eq!(stored.value, Voltage::from_mv(1_250).into());
    assert!(stored.expiry.expect("a reading expires").stamp_ms() > first.stamp_ms());
}

/// Serialises the tests that fill the shared channel.
static CHANNEL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn empty_channel() {
    while METRICS.try_receive().is_ok() {}
}

// Pushes past the depth of the channel with no reader are counted.
#[test]
fn a_push_to_a_full_channel_is_counted() {
    let _guard = CHANNEL.lock().expect("no test panics holding the lock");
    empty_channel();
    let before = dropped();
    let excess = 5_u32;
    for _ in 0..Metrics::CHANNEL_DEPTH + excess as usize {
        metric!(CHIP_TEMPERATURE, ANY_TEMPERATURE).try_push();
    }
    assert_eq!(dropped() - before, excess);
    empty_channel();
}

// The longest chain a board holds, with both quantities of each chip,
// fits a map of 512 series and loses no push when the reader keeps up.
#[test]
fn the_longest_chain_fits_a_map_of_512_series() {
    let _guard = CHANNEL.lock().expect("no test panics holding the lock");
    empty_channel();
    let before = dropped();
    let mut map = heapless::index_map::FnvIndexMap::<MetricKey, StoredValue, 512>::new();
    for chip in 0..=u8::MAX {
        metric!(CHIP_TEMPERATURE, ANY_TEMPERATURE)
            .label(HASHBOARD_LABEL, HashboardIndex(0))
            .label(CHIP_ADDR_LABEL, chip)
            .try_push();
        metric!("chip_voltage_volts", Voltage::from_mv(1_200))
            .label(HASHBOARD_LABEL, HashboardIndex(0))
            .label(CHIP_ADDR_LABEL, chip)
            .try_push();
        while let Ok(metric) = METRICS.try_receive() {
            upsert_metric(&mut map, &metric);
        }
    }
    assert_eq!(map.len(), 512);
    assert_eq!(dropped(), before);
}

fn dropped_count(map: &Map) -> Option<u64> {
    map.iter()
        .find(|(key, _)| key.0.as_str() == "metrics_dropped")
        .and_then(|(_, stored)| match stored.value {
            Value::Count(count) => Some(count),
            _ => None,
        })
}

// The first write has no earlier count, so the series exists with the
// count at that time before any push can fill the map.
#[test]
fn the_first_report_writes_the_drop_count() {
    let mut map = Map::new();
    assert_eq!(report_dropped(&mut map, 0, None), Some(0));
    assert_eq!(dropped_count(&map), Some(0));
}

#[test]
fn a_report_writes_the_count_when_it_changed() {
    let mut map = Map::new();
    let reported = report_dropped(&mut map, 0, None);
    assert_eq!(report_dropped(&mut map, 3, reported), Some(3));
    assert_eq!(dropped_count(&map), Some(3));
}

// A count that did not change is not written again, so a full map is
// not searched for a slot after every push.
#[test]
fn a_report_of_the_same_count_writes_nothing() {
    let mut map = Map::new();
    assert_eq!(report_dropped(&mut map, 2, Some(2)), Some(2));
    assert_eq!(dropped_count(&map), None);
}

// The series is a constant, so the slot of the first write survives a
// full map that drops its expired series.
#[test]
fn the_drop_count_keeps_its_slot_in_a_full_map() {
    let mut map = Map::new();
    report_dropped(&mut map, 0, None);
    upsert(&mut map, "stopped", Duration::from_secs(1));
    block_for(Duration::from_millis(1_100));
    upsert(&mut map, "new", Duration::from_secs(3_600));
    assert_eq!(dropped_count(&map), Some(0));
    assert!(holds(&map, "new") && !holds(&map, "stopped"));
}
