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

//! Unit tests for the dangerous temperature flag.

/// The I2C address of the sensor that the hashboard readings name.
const SENSOR: u8 = 0x48;

use super::*;
use ubos_core::index::{ChipAddress, HashboardIndex};

const fn degrees(celsius: i32) -> Temperature {
    Temperature::from_celsius(celsius)
}

fn chip(celsius: i32) -> Reading {
    Reading {
        source: TempSource::chip(HashboardIndex(0), ChipAddress::One(0)),
        value: degrees(celsius),
    }
}

const fn dangerous() -> TemperatureDescriptor {
    TemperatureDescriptor {
        chip: Some(degrees(90)),
        hashboard: Some(degrees(80)),
    }
}

fn flag(watch: &FlagWatch) -> bool {
    watch.try_get().expect("the flag was published")
}

#[test]
fn a_reading_at_its_limit_sets_the_flag_and_one_below_does_not() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    danger.on_reading(chip(89));
    assert_eq!(WATCH.try_get(), None);
    danger.on_reading(chip(90));
    assert!(flag(&WATCH));
}

#[test]
fn a_kind_with_no_limit_never_sets_the_flag() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let ambient = Reading {
        source: TempSource::Ambient,
        value: degrees(200),
    };
    danger.on_reading(ambient);
    danger.set_limits(TemperatureDescriptor {
        chip: None,
        hashboard: Some(degrees(80)),
    });
    danger.on_reading(chip(200));
    assert_eq!(WATCH.try_get(), None);
}

#[test]
fn a_hot_reading_sets_the_flag_at_once() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    danger.publish();
    assert!(!flag(&WATCH));
    danger.on_reading(chip(95));
    assert!(flag(&WATCH));
}

#[test]
fn a_tick_clears_the_flag_when_the_fresh_temperature_cooled() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let mut temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    temperatures.update(chip(95));
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH), "still hot");
    temperatures.end_sample();
    temperatures.update(chip(40));
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH), "the hot pane is still inside the window");
    for _ in 0..2 {
        temperatures.end_sample();
        temperatures.update(chip(40));
    }
    danger.on_tick(&temperatures, degrees(45));
    assert!(!flag(&WATCH));
}

#[test]
fn a_kind_with_no_limit_does_not_hold_the_flag_up() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let mut temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    temperatures.update(chip(40));
    temperatures.update(Reading {
        source: TempSource::ControlBoard,
        value: degrees(70),
    });
    danger.on_tick(&temperatures, degrees(45));
    danger.on_tick(&temperatures, degrees(45));
    assert!(!flag(&WATCH));
}

#[test]
fn the_tick_of_a_hot_chip_does_not_clear_the_flag_against_cool_dies() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let mut temperatures = Temperatures::new();
    temperatures.update(chip(40));
    danger.on_reading(chip(95));
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH), "the hot die is in no filter");
    // The flag is set already, and a second hot die still holds it.
    danger.on_reading(chip(95));
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH));
    danger.on_tick(&temperatures, degrees(45));
    assert!(
        !flag(&WATCH),
        "a tick with no hot reading judges the filter"
    );
}

#[test]
fn a_kind_still_hot_holds_the_flag_though_the_other_cooled() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let mut temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    temperatures.update(chip(40));
    temperatures.update(Reading {
        source: TempSource::hashboard(HashboardIndex(0), SENSOR),
        value: degrees(70),
    });
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH));
}

#[test]
fn a_cool_kind_does_not_clear_the_flag_that_an_unread_kind_set() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let mut temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    temperatures.update(Reading {
        source: TempSource::hashboard(HashboardIndex(0), SENSOR),
        value: degrees(40),
    });
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH), "no chip has a reading");
    danger.acknowledge();
    assert!(!flag(&WATCH));
}

#[test]
fn silence_does_not_clear_the_flag() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH));
}

#[test]
fn the_acknowledge_clears_the_flag_and_a_hot_reading_sets_it_again() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    danger.on_tick(&temperatures, degrees(45));
    danger.acknowledge();
    assert!(!flag(&WATCH));
    danger.on_reading(chip(95));
    assert!(flag(&WATCH));
}

#[test]
fn the_acknowledge_clears_the_flag_though_a_fresh_value_is_at_its_limit() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let mut temperatures = Temperatures::new();
    let hashboard = Reading {
        source: TempSource::hashboard(HashboardIndex(0), SENSOR),
        value: degrees(85),
    };
    danger.on_reading(hashboard);
    temperatures.update(hashboard);
    danger.on_tick(&temperatures, degrees(45));
    assert!(flag(&WATCH));
    danger.acknowledge();
    assert!(!flag(&WATCH), "the operator decides");
    danger.on_reading(hashboard);
    assert!(flag(&WATCH), "the next hot reading sets it again");
}

#[test]
fn the_acknowledge_leaves_the_flag_set_when_a_reading_at_its_limit_came_this_tick() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let temperatures = Temperatures::new();
    danger.on_reading(chip(95));
    danger.on_tick(&temperatures, degrees(45));
    danger.on_reading(chip(95));
    danger.acknowledge();
    assert!(flag(&WATCH));
    danger.on_tick(&temperatures, degrees(45));
    danger.acknowledge();
    assert!(!flag(&WATCH));
}

#[test]
fn a_subscriber_wakes_once_for_each_edge() {
    static WATCH: FlagWatch = FlagWatch::new();
    let mut danger = Danger::new(&WATCH, dangerous());
    let temperatures = Temperatures::new();
    let mut receiver = WATCH.receiver().expect("a free receiver");
    danger.on_reading(chip(95));
    assert_eq!(receiver.try_changed(), Some(true));
    danger.on_reading(chip(95));
    assert_eq!(receiver.try_changed(), None);
    danger.on_tick(&temperatures, degrees(45));
    danger.acknowledge();
    assert_eq!(receiver.try_changed(), Some(false));
    danger.acknowledge();
    assert_eq!(receiver.try_changed(), None);
}
