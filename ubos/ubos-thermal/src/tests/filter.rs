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

//! Unit tests for the window filter. They drive the filter with plain
//! instants, so no embassy executor or clock is involved.

use super::*;

/// The sample interval of the filters, which gives panes of 500 ms.
const INTERVAL: Duration = Duration::from_millis(1000);

const fn degrees(celsius: i32) -> Temperature {
    Temperature::from_celsius(celsius)
}

const fn at(millis: u64) -> Instant {
    Instant::from_millis(millis)
}

#[test]
fn a_burst_of_four_sensors_gives_the_hottest_and_the_mean() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    for (index, celsius) in [(0, 60), (1, 64), (2, 58), (3, 62)] {
        filter.update(degrees(celsius), at(10 * index));
    }
    let stats = filter.value(at(100)).expect("four readings are inside");
    assert_eq!(stats.max, degrees(64));
    assert_eq!(stats.mean, degrees(61));
}

#[test]
fn a_rising_ramp_never_reads_below_the_last_reading() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    for second in 0..60u64 {
        let celsius = 40 + i32::try_from(second).expect("BUG: 60 fits an i32");
        filter.update(degrees(celsius), at(1000 * second));
        let stats = filter.value(at(1000 * second)).expect("a reading is in");
        assert!(stats.max >= degrees(celsius));
    }
}

#[test]
fn a_falling_ramp_reads_high_by_at_most_two_samples() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    for second in 0..60u64 {
        let celsius = 100 - i32::try_from(second).expect("BUG: 60 fits an i32");
        filter.update(degrees(celsius), at(1000 * second));
        let stats = filter.value(at(1000 * second)).expect("a reading is in");
        assert!(stats.max <= degrees(celsius + 2));
    }
}

#[test]
fn a_silent_source_gives_none_after_the_window() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    filter.update(degrees(60), at(0));
    assert!(filter.value(at(1000)).is_some(), "still inside the window");
    assert!(
        filter.value(at(2000)).is_some(),
        "one missed poll leaves the reading inside the window"
    );
    assert!(
        filter.value(at(2500)).is_some(),
        "a tick that is late across a pane boundary still reads it"
    );
    assert_eq!(filter.value(at(3000)), None, "six widths have passed");
}

#[test]
fn a_pane_that_comes_round_again_starts_afresh() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    filter.update(degrees(90), at(0));
    // Six widths later the same pane holds a new epoch.
    filter.update(degrees(50), at(3000));
    let stats = filter.value(at(3000)).expect("a reading is in");
    assert_eq!(stats.max, degrees(50), "the old reading is gone");
}

#[test]
fn six_hundred_chips_fit_in_the_fixed_memory() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    let size = core::mem::size_of::<TempFilter>();
    for index in 0..600i32 {
        filter.update(degrees(40 + index / 20), at(0));
    }
    assert_eq!(core::mem::size_of::<TempFilter>(), size);
    let stats = filter.value(at(0)).expect("readings are in");
    assert_eq!(stats.max, degrees(69));
    assert_eq!(stats.mean, Temperature::from_millicelsius(54_500));
}

#[test]
fn the_stats_of_a_tick_count_as_one_reading() {
    let mut filter = TempFilter::for_interval(INTERVAL);
    let tick = |max, mean| TempStats {
        max: degrees(max),
        mean: degrees(mean),
    };
    filter.update_stats(tick(70, 60), at(0));
    filter.update_stats(tick(66, 64), at(1000));
    let stats = filter.value(at(1000)).expect("two ticks are inside");
    assert_eq!(stats.max, degrees(70));
    assert_eq!(stats.mean, degrees(62));
}
