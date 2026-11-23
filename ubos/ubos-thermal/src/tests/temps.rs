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

//! Unit tests for the published temperatures.

/// The I2C address of the sensor that the hashboard readings name.
const SENSOR: u8 = 0x48;

use super::*;
use crate::TempStats;
use ubos_config::{Source, Statistic};
use ubos_core::index::{ChipAddress, HashboardIndex};

/// The sample interval of the filters, which gives panes of 500 ms.
const INTERVAL: Duration = Duration::from_millis(1000);

const fn reading(source: TempSource, celsius: i32) -> Reading {
    Reading {
        source,
        value: Temperature::from_celsius(celsius),
    }
}

const fn stats(max: i32, mean: i32) -> TempStats {
    TempStats {
        max: Temperature::from_celsius(max),
        mean: Temperature::from_celsius(mean),
    }
}

/// The value that `watch` carries, with a panic when nothing was
/// published to it.
fn published(watch: &TempWatch) -> Option<Temperature> {
    watch.try_get().expect("a value was published")
}

#[test]
fn each_reading_reaches_the_watch_of_its_source() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    let now = Instant::from_millis(0);
    let chain = HashboardIndex(0);
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(1)), 80),
        now,
    );
    temperatures.update(reading(TempSource::hashboard(chain, SENSOR), 60), now);
    temperatures.update(reading(TempSource::Ambient, 25), now);
    temperatures.publish_filtered(now, &WATCHES);
    temperatures.publish_ambient(now, &WATCHES);
    let degrees = |celsius| Some(Temperature::from_celsius(celsius));
    assert_eq!(
        WATCHES.filtered.try_get().flatten(),
        Some(TempSample {
            chip: Some(stats(80, 80)),
            hashboard: Some(stats(60, 60)),
        })
    );
    assert_eq!(published(&WATCHES.ambient), degrees(25));
}

#[test]
fn the_sample_carries_the_hottest_and_the_mean_of_a_kind() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    let now = Instant::from_millis(0);
    let chain = HashboardIndex(0);
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(0)), 80),
        now,
    );
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(1)), 70),
        now,
    );
    temperatures.publish_filtered(now, &WATCHES);
    temperatures.publish_ambient(now, &WATCHES);
    assert_eq!(
        WATCHES.filtered.try_get().flatten(),
        Some(TempSample {
            chip: Some(stats(80, 75)),
            hashboard: None,
        })
    );
}

#[test]
fn a_sample_with_neither_kind_inside_its_window_is_none() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    let chain = HashboardIndex(0);
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(0)), 80),
        Instant::from_millis(0),
    );
    temperatures.publish_filtered(Instant::from_millis(3000), &WATCHES);
    temperatures.publish_ambient(Instant::from_millis(3000), &WATCHES);
    assert_eq!(WATCHES.filtered.try_get(), Some(None));
}

#[test]
fn a_reading_with_no_filter_reaches_no_watch() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    let now = Instant::from_millis(0);
    temperatures.update(reading(TempSource::ControlBoard, 50), now);
    temperatures.publish_filtered(now, &WATCHES);
    temperatures.publish_ambient(now, &WATCHES);
    assert_eq!(WATCHES.filtered.try_get(), Some(None));
    assert_eq!(published(&WATCHES.ambient), None);
}

#[test]
fn a_silent_source_publishes_none() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    temperatures.update(reading(TempSource::Ambient, 25), Instant::from_millis(0));
    temperatures.publish_filtered(Instant::from_millis(3000), &WATCHES);
    temperatures.publish_ambient(Instant::from_millis(3000), &WATCHES);
    assert_eq!(published(&WATCHES.ambient), None);
}

/// A target that follows `statistic` of `source`. Thermal reads no
/// setpoint, so any temperature serves.
const fn target(source: Source, statistic: Statistic) -> PidTarget {
    PidTarget {
        source,
        statistic,
        temp: Temperature::from_celsius(0),
    }
}

#[test]
fn the_pid_temperature_is_the_statistic_of_the_target_over_its_kind() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    let now = Instant::from_millis(0);
    let chain = HashboardIndex(0);
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(0)), 90),
        now,
    );
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(1)), 70),
        now,
    );
    temperatures.update(reading(TempSource::hashboard(chain, SENSOR), 60), now);
    let pid = |source, statistic, celsius| PidTemperature {
        source,
        statistic,
        temp: Some(Temperature::from_celsius(celsius)),
    };

    temperatures.publish_pid(now, target(Source::Chip, Statistic::Max), &WATCHES);
    assert_eq!(
        WATCHES.pid.try_get(),
        Some(pid(Source::Chip, Statistic::Max, 90))
    );
    temperatures.publish_pid(now, target(Source::Chip, Statistic::Mean), &WATCHES);
    assert_eq!(
        WATCHES.pid.try_get(),
        Some(pid(Source::Chip, Statistic::Mean, 80))
    );
    // The hashboard, even beside a hotter die.
    temperatures.publish_pid(now, target(Source::Hashboard, Statistic::Max), &WATCHES);
    assert_eq!(
        WATCHES.pid.try_get(),
        Some(pid(Source::Hashboard, Statistic::Max, 60))
    );
}

#[test]
fn the_pid_temperature_is_none_without_a_reading_of_the_target_kind() {
    static WATCHES: Watches = Watches::new();
    let mut temperatures = Temperatures::for_interval(INTERVAL);
    let now = Instant::from_millis(0);
    let chain = HashboardIndex(0);
    temperatures.update(
        reading(TempSource::chip(chain, ChipAddress::One(0)), 90),
        now,
    );

    // The die is never handed to a target that follows the hashboard.
    temperatures.publish_pid(now, target(Source::Hashboard, Statistic::Max), &WATCHES);
    assert_eq!(WATCHES.pid.try_get().and_then(|pid| pid.temp), None);

    // The chip reading has left its window.
    let later = Instant::from_millis(3000);
    temperatures.publish_pid(later, target(Source::Chip, Statistic::Max), &WATCHES);
    assert_eq!(WATCHES.pid.try_get().and_then(|pid| pid.temp), None);
}
