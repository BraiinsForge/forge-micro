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

//! Unit tests for the window filter.

use super::*;

const fn degrees(celsius: i32) -> Temperature {
    Temperature::from_celsius(celsius)
}

#[test]
fn a_burst_of_four_sensors_gives_the_hottest_and_the_mean() {
    let mut filter = TempFilter::new();
    for celsius in [60, 64, 58, 62] {
        filter.update(degrees(celsius));
    }
    let stats = filter.value().expect("four readings are inside");
    assert_eq!(stats.max, degrees(64));
    assert_eq!(stats.mean, degrees(61));
}

#[test]
fn a_rising_ramp_never_reads_below_the_last_reading() {
    let mut filter = TempFilter::new();
    for celsius in 40..100 {
        filter.update(degrees(celsius));
        let stats = filter.value().expect("a reading is in");
        assert!(stats.max >= degrees(celsius));
        filter.end_sample();
    }
}

#[test]
fn a_falling_ramp_reads_high_by_at_most_two_samples() {
    let mut filter = TempFilter::new();
    for celsius in (40..100).rev() {
        filter.update(degrees(celsius));
        let stats = filter.value().expect("a reading is in");
        assert!(stats.max <= degrees(celsius + 2));
        filter.end_sample();
    }
}

#[test]
fn a_silent_source_gives_none_after_the_window() {
    let mut filter = TempFilter::new();
    filter.update(degrees(60));
    filter.end_sample();
    assert!(
        filter.value().is_some(),
        "one missed read keeps the reading"
    );
    filter.end_sample();
    assert!(filter.value().is_some(), "and so does a second");
    filter.end_sample();
    assert_eq!(filter.value(), None, "the third sample leaves it behind");
}

#[test]
fn a_pane_that_comes_round_again_starts_afresh() {
    let mut filter = TempFilter::new();
    filter.update(degrees(90));
    for _ in 0..SAMPLES {
        filter.end_sample();
    }
    filter.update(degrees(50));
    let stats = filter.value().expect("a reading is in");
    assert_eq!(stats.max, degrees(50), "the old reading is gone");
}

#[test]
fn six_hundred_chips_fit_in_the_fixed_memory() {
    let mut filter = TempFilter::new();
    let size = core::mem::size_of::<TempFilter>();
    for index in 0..600i32 {
        filter.update(degrees(40 + index / 20));
    }
    assert_eq!(core::mem::size_of::<TempFilter>(), size);
    let stats = filter.value().expect("readings are in");
    assert_eq!(stats.max, degrees(69));
    assert_eq!(stats.mean, Temperature::from_millicelsius(54_500));
}

#[test]
fn the_stats_of_a_sample_count_as_one_reading() {
    let mut filter = TempFilter::new();
    let sample = |max, mean| TempStats {
        max: degrees(max),
        mean: degrees(mean),
    };
    filter.update_stats(sample(70, 60));
    filter.end_sample();
    filter.update_stats(sample(66, 64));
    let stats = filter.value().expect("two samples are inside");
    assert_eq!(stats.max, degrees(70));
    assert_eq!(stats.mean, degrees(62));
}
