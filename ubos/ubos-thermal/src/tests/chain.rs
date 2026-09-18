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
use portable_atomic::AtomicU8;
use ubos_metrics::chain::ChainPresence;
use ubos_metrics::chain::ChainStats;

const TEST_CHIPS: usize = 4;
const TEST_CHAINS: usize = 2;

fn stores() -> &'static [ChainPvt<TEST_CHIPS>; TEST_CHAINS] {
    std::boxed::Box::leak(std::boxed::Box::new([ChainPvt::new(), ChainPvt::new()]))
}

/// A board's accounting table, leaked so each test owns one rather than
/// sharing the table every board runs on.
fn stats() -> ChainStatsHandle {
    ChainStatsHandle::new(
        std::boxed::Box::leak(std::boxed::Box::new([ChainStats::new(), ChainStats::new()])),
        std::boxed::Box::leak(std::boxed::Box::new(AtomicU8::new(
            u8::try_from(TEST_CHAINS).expect("BUG: TEST_CHAINS fits a u8"),
        ))),
    )
}

/// A board whose chains are both present, which is what the miner
/// records once every connector has enumerated its expected count.
fn present_board() -> (
    &'static [ChainPvt<TEST_CHIPS>; TEST_CHAINS],
    ChainStatsHandle,
) {
    let stats = stats();
    for chain in stats.chains() {
        chain.set_presence(ChainPresence::Present { chips: 4 });
    }
    (stores(), stats)
}

#[test]
fn a_chain_that_has_answered_nothing_falls_silent_on_the_clock() {
    let (pvt, stats) = present_board();
    let sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);
    // A chain still enumerating has answered nothing and is not yet
    // silent; the window runs from where the sensor started.
    assert!(!sensor.chain_is_silent(0, sensor.started));
    assert!(sensor.chain_is_silent(0, sensor.started + MAX_SILENCE));
}

#[test]
fn one_chip_answering_keeps_the_whole_chain_fresh() {
    let (pvt, stats) = present_board();
    pvt[1].record(2, Some(55.5), None);
    let sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);
    let now = Instant::now();
    assert!(!sensor.chain_is_silent(1, now));
    // Three chips have never answered, and the reading stands on the one
    // that did.
    assert_eq!(pvt[1].hottest_millicelsius(), Some(55_500));
}

#[test]
fn a_failed_read_does_not_hold_the_chain_open() {
    let (pvt, stats) = present_board();
    let sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);
    // The answer lands after the sensor starts, so silence is measured
    // from the answer and not from the start.
    pvt[0].record(0, Some(40.0), None);
    let answered = pvt[0].last_answer().expect("chip 0 answered");
    // Reads that carry nothing keep arriving; the clock still runs from
    // the last real reading.
    pvt[0].record(0, None, None);
    assert!(sensor.chain_is_silent(0, answered + MAX_SILENCE));
}

#[test]
fn the_sample_is_the_hottest_die_of_every_present_chain() {
    let (pvt, stats) = present_board();
    pvt[0].record(0, Some(60.0), None);
    pvt[1].record(3, Some(72.0), None);
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);

    let sample = embassy_futures::block_on(sensor.sample()).expect("both chains reported");
    assert_eq!(sample.safety.millicelsius(), 72_000);
    // Each chain's own hottest die feeds its dashboard column.
    assert_eq!(stats.chain(0).hottest_mc(), Some(60_000));
    assert_eq!(stats.chain(1).hottest_mc(), Some(72_000));
}

#[test]
fn a_present_chain_with_no_reading_does_not_hold_back_a_hot_sample() {
    let (pvt, stats) = present_board();
    // Chain 0 has answered hot; chain 1 has never answered at all. The
    // sample must not wait on chain 1 while chain 0 already holds a
    // reading that may need the trip.
    pvt[0].record(0, Some(60.0), None);
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);

    let sample = embassy_futures::block_on(sensor.sample()).expect("chain 0 reported");
    assert_eq!(sample.safety.millicelsius(), 60_000);
    // Chain 1's silence still reaches the dashboard through its own
    // accounting, even though it did not block the sample.
    assert_eq!(stats.chain(1).hottest_mc(), None);
}

#[test]
fn no_present_chain_with_any_reading_yields_no_sample() {
    let (pvt, stats) = present_board();
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);

    // Neither chain has ever answered, so there is no hot reading to
    // report and nothing to trip a halt on yet.
    assert!(embassy_futures::block_on(sensor.sample()).is_none());
    assert!(!sensor.halts_on_fault());
}

#[test]
fn an_absent_chain_is_skipped_rather_than_read_as_silent() {
    let (pvt, stats) = present_board();
    stats.chain(1).set_presence(ChainPresence::Absent);
    stats.chain(1).set_hottest(Some(70_000));
    pvt[0].record(0, Some(60.0), None);
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);

    let sample = embassy_futures::block_on(sensor.sample()).expect("chain 0 carries the board");
    assert_eq!(sample.safety.millicelsius(), 60_000);
    // The connector holds no board, so its column clears rather than
    // standing at whatever it last read.
    assert_eq!(stats.chain(1).hottest_mc(), None);
    // The interlock arms on the first reading, and this was one.
    assert!(sensor.halts_on_fault());
}

#[test]
fn a_board_with_no_present_chain_never_halts() {
    let (pvt, stats) = present_board();
    for chain in stats.chains() {
        chain.set_presence(ChainPresence::Unknown);
    }
    pvt[0].record(0, Some(60.0), None);
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);

    // Nothing has enumerated yet, so there is no chain to read and no
    // fault to report.
    assert!(embassy_futures::block_on(sensor.sample()).is_none());
    assert!(!sensor.halts_on_fault());
}

#[test]
fn a_chain_present_again_after_a_halt_is_not_silent_on_the_pre_halt_answer() {
    let (pvt, stats) = present_board();
    pvt[0].record(0, Some(60.0), None);
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);
    let pre_halt_answer = pvt[0].last_answer().expect("chip 0 answered");

    // The chain drops out for longer than MAX_SILENCE, then comes back.
    let dropped_at = pre_halt_answer + Duration::from_secs(1);
    sensor.track_presence(0, dropped_at, false);
    let back_at = dropped_at + MAX_SILENCE + Duration::from_secs(1);
    sensor.track_presence(0, back_at, true);

    // Silence must be measured from `back_at`, not from the answer that
    // predates the halt, or the chain would read as already expired on
    // its first tick back.
    assert!(!sensor.chain_is_silent(0, back_at));
    assert!(sensor.chain_is_silent(0, back_at + MAX_SILENCE));
}

#[test]
fn chain_voltage_requires_every_chip_and_converts_microvolts_to_millivolts() {
    let (pvt, stats) = present_board();
    let sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);
    assert_eq!(sensor.chain_voltage_mv(), None);
    for chip in 0..3 {
        pvt[0].record(chip, Some(50.0), Some(303.0));
    }
    // A chain short of one chip reads as severe rail droop, so it is
    // skipped rather than summed.
    assert_eq!(sensor.chain_voltage_mv(), None);
    pvt[0].record(3, Some(50.0), Some(306.0));
    assert_eq!(sensor.chain_voltage_mv(), Some(1215));
}

#[test]
fn chain_voltage_takes_the_lowest_chain_that_reported_every_chip() {
    let (pvt, stats) = present_board();
    let sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);
    for chip in 0..u8::try_from(TEST_CHIPS).expect("the test chain is small") {
        pvt[0].record(chip, Some(50.0), Some(300.0));
        pvt[1].record(chip, Some(50.0), Some(310.0));
    }
    // Both chains hang off one rail, so the lower sum is the worst-case
    // reading: the one showing the board's droop.
    assert_eq!(sensor.chain_voltage_mv(), Some(1200));
}

#[test]
fn a_chain_silent_for_a_second_sample_stops_the_sensor_answering() {
    let (pvt, stats) = present_board();
    pvt[0].record(0, Some(60.0), None);
    let mut sensor = ChainTemperature::<TEST_CHIPS, TEST_CHAINS>::new(pvt, stats);

    // The first silent sample of chain 1 still delivers chain 0's reading.
    sensor.silent_ticks[1] = SILENT_TICKS_BEFORE_FAULT - 1;
    assert!(!sensor.stalled());
    // One more silent sample and the sensor stops answering, so the loop
    // counts misses towards its fault the way it does for one chain.
    sensor.silent_ticks[1] = SILENT_TICKS_BEFORE_FAULT;
    assert!(sensor.stalled());
}
