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
use crate::chain_readings::ChainReading;
use crate::{TempStats, clock};
use portable_atomic::{AtomicBool, Ordering};
use ubos_core::index::ChipAddress;
use ubos_core::units::{Temperature, Voltage};

const TEST_CHIPS: usize = 4;
const TEST_CHAINS: usize = 2;

/// One sample of `sensor`, and the dies it submitted, if any. The
/// channel is shared by every test, so it is emptied first, and the
/// clock lock that each test holds keeps the others off it meanwhile.
fn sampled<S: TemperatureSensor>(sensor: &mut S) -> Option<TempStats> {
    while crate::INPUT.try_receive().is_ok() {}
    embassy_futures::block_on(sensor.sample());
    let mut dies = None;
    while let Ok(submitted) = crate::INPUT.try_receive() {
        match submitted {
            crate::Submitted::Dies(stats) => dies = Some(stats),
            other => panic!("a chain sensor submits its dies only, got {other:?}"),
        }
    }
    dies
}

fn stores() -> &'static [ChainPvt<TEST_CHIPS>; TEST_CHAINS] {
    std::boxed::Box::leak(std::boxed::Box::new([ChainPvt::new(), ChainPvt::new()]))
}

/// What the miner knows of a board and what the sensor reads off it,
/// leaked so each test owns one rather than sharing the board's own.
#[derive(Clone, Copy)]
struct Board {
    present: &'static [AtomicBool; TEST_CHAINS],
    readings: ChainReadings,
}

impl Board {
    fn new() -> Self {
        let table: &'static [ChainReading; TEST_CHAINS] =
            std::boxed::Box::leak(std::boxed::Box::new([
                ChainReading::new(),
                ChainReading::new(),
            ]));
        Self {
            present: std::boxed::Box::leak(std::boxed::Box::new([
                AtomicBool::new(false),
                AtomicBool::new(false),
            ])),
            readings: ChainReadings::new(table),
        }
    }

    fn set_present(&self, chain: HashboardIndex, present: bool) {
        self.present[chain.slot()].store(present, Ordering::Relaxed);
    }

    fn set_all_present(&self, present: bool) {
        for chain in HashboardIndex::range(TEST_CHAINS) {
            self.set_present(chain, present);
        }
    }
}

impl PresentChains for Board {
    fn is_present(&self, chain: HashboardIndex) -> bool {
        self.present[chain.slot()].load(Ordering::Relaxed)
    }
}

/// The sensor of `board`, writing into the readings of that board.
fn sensor(
    pvt: &'static [ChainPvt<TEST_CHIPS>; TEST_CHAINS],
    board: Board,
) -> ChainTemperature<Board, TEST_CHIPS, TEST_CHAINS> {
    let mut sensor = ChainTemperature::new(pvt, board, crate::Handle(()));
    sensor.readings = board.readings;
    sensor
}

/// A board whose chains are both present, which is what the miner
/// records once every connector has enumerated its expected count.
///
/// Holds the mock clock still for the test, because every sensor here
/// measures silence against `Instant::now` and the loop tests reset
/// that clock. The guard rides along so a test cannot take the board
/// without the clock.
fn present_board() -> (
    std::sync::MutexGuard<'static, ()>,
    &'static [ChainPvt<TEST_CHIPS>; TEST_CHAINS],
    Board,
) {
    let clock = clock();
    let stats = Board::new();
    stats.set_all_present(true);
    (clock, stores(), stats)
}

#[test]
fn a_chain_that_has_answered_nothing_falls_silent_on_the_clock() {
    let (_clock, pvt, stats) = present_board();
    let sensor = sensor(pvt, stats);
    // A chain still enumerating has answered nothing and is not yet
    // silent; the window runs from where the sensor started.
    assert!(!sensor.chain_is_silent(HashboardIndex(0), sensor.started));
    assert!(sensor.chain_is_silent(HashboardIndex(0), sensor.started + MAX_SILENCE));
}

#[test]
fn one_chip_answering_keeps_the_whole_chain_fresh() {
    let (_clock, pvt, stats) = present_board();
    pvt[1].record(
        ChipAddress::One(2),
        Some(Temperature::from_celsius_f64(55.5)),
        None,
    );
    let sensor = sensor(pvt, stats);
    let now = Instant::now();
    assert!(!sensor.chain_is_silent(HashboardIndex(1), now));
    // Three chips have never answered, and the reading stands on the one
    // that did.
    assert_eq!(
        pvt[1].hottest().map(Temperature::millicelsius),
        Some(55_500)
    );
}

#[test]
fn a_failed_read_does_not_hold_the_chain_open() {
    let (_clock, pvt, stats) = present_board();
    let sensor = sensor(pvt, stats);
    // The answer lands after the sensor starts, so silence is measured
    // from the answer and not from the start.
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(40.0)),
        None,
    );
    let answered = pvt[0].last_answer().expect("chip 0 answered");
    // Reads that carry nothing keep arriving; the clock still runs from
    // the last real reading.
    pvt[0].record(ChipAddress::One(0), None, None);
    assert!(sensor.chain_is_silent(HashboardIndex(0), answered + MAX_SILENCE));
}

#[test]
fn the_sample_is_the_hottest_and_the_mean_die_of_every_present_chain() {
    let (_clock, pvt, stats) = present_board();
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    pvt[0].record(
        ChipAddress::One(1),
        Some(Temperature::from_celsius_f64(63.0)),
        None,
    );
    pvt[1].record(
        ChipAddress::One(3),
        Some(Temperature::from_celsius_f64(72.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);

    assert_eq!(
        sampled(&mut sensor),
        Some(TempStats {
            max: Temperature::from_millicelsius(72_000),
            mean: Temperature::from_millicelsius(65_000),
        })
    );
    // Each chain's own hottest die feeds its dashboard column.
    assert_eq!(
        stats.readings.hottest(HashboardIndex(0)),
        Some(Temperature::from_celsius(63))
    );
    assert_eq!(
        stats.readings.hottest(HashboardIndex(1)),
        Some(Temperature::from_celsius(72))
    );
}

#[test]
fn a_present_chain_with_no_reading_does_not_hold_back_a_hot_sample() {
    let (_clock, pvt, stats) = present_board();
    // Chain 0 has answered hot; chain 1 has never answered at all. The
    // sample must not wait on chain 1 while chain 0 already holds a
    // reading that may need the trip.
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);

    let dies = sampled(&mut sensor);
    assert_eq!(dies.map(|dies| dies.max.millicelsius()), Some(60_000));
    // Chain 1's silence still reaches the dashboard through its own
    // accounting, even though it did not block the sample.
    assert_eq!(stats.readings.hottest(HashboardIndex(1)), None);
}

#[test]
fn present_chains_that_answered_nothing_submit_nothing() {
    let (_clock, pvt, stats) = present_board();
    let mut sensor = sensor(pvt, stats);

    // Both chains are present and neither has answered. The miner marks
    // a chain present only after its PVT sweep succeeded, so this state
    // does not arise on a real board, and where it does the chips are
    // powered and owe a reading.
    assert_eq!(sampled(&mut sensor), None);
}

#[test]
fn an_absent_chain_is_skipped_rather_than_read_as_silent() {
    let (_clock, pvt, stats) = present_board();
    stats.set_present(HashboardIndex(1), false);
    stats
        .readings
        .set_hottest(HashboardIndex(1), Some(Temperature::from_celsius(70)));
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);

    let dies = sampled(&mut sensor);
    assert_eq!(dies.map(|dies| dies.max.millicelsius()), Some(60_000));
    // The connector holds no board, so its column clears rather than
    // standing at whatever it last read.
    assert_eq!(stats.readings.hottest(HashboardIndex(1)), None);
}

#[test]
fn a_board_with_no_present_chain_submits_nothing() {
    let (_clock, pvt, stats) = present_board();
    stats.set_all_present(false);
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);

    // This is what a rail cut leaves behind, and what a board looks like
    // before its first bring-up. A stale reading sits in the store, and
    // it must not be reported and must not count as a fault.
    assert_eq!(sampled(&mut sensor), None);
}

#[test]
fn a_halt_that_cuts_the_rail_does_not_report_a_fault() {
    let (_clock, pvt, stats) = present_board();
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);
    let dies = sampled(&mut sensor);
    assert_eq!(dies.map(|dies| dies.max.millicelsius()), Some(60_000));

    // `cut_rail` marks every present chain unknown. The sensor must read
    // the silence that follows as an idle board and not as a fault, or
    // the halt holds itself down for ever.
    stats.set_all_present(false);
    assert_eq!(sampled(&mut sensor), None);
}

#[test]
fn a_chain_present_again_after_a_halt_is_not_silent_on_the_pre_halt_answer() {
    let (_clock, pvt, stats) = present_board();
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);
    let pre_halt_answer = pvt[0].last_answer().expect("chip 0 answered");

    // The chain drops out for longer than MAX_SILENCE, then comes back.
    let dropped_at = pre_halt_answer + Duration::from_secs(1);
    sensor.track_presence(HashboardIndex(0), dropped_at, false);
    let back_at = dropped_at + MAX_SILENCE + Duration::from_secs(1);
    sensor.track_presence(HashboardIndex(0), back_at, true);

    // Silence must be measured from `back_at`, not from the answer that
    // predates the halt, or the chain would read as already expired on
    // its first tick back.
    assert!(!sensor.chain_is_silent(HashboardIndex(0), back_at));
    assert!(sensor.chain_is_silent(HashboardIndex(0), back_at + MAX_SILENCE));
}

#[test]
fn the_voltage_of_a_chain_requires_every_chip_and_sums_them() {
    let (_clock, pvt, stats) = present_board();
    let sensor = sensor(pvt, stats);
    let voltage = || sensor.voltage_of(HashboardIndex(0), Instant::now());
    assert_eq!(voltage(), None);
    for chip in ChipAddress::range(3) {
        pvt[0].record(
            chip,
            Some(Temperature::from_celsius_f64(50.0)),
            Some(Voltage::from_mv(303)),
        );
    }
    // A chain short of one chip reads as severe rail droop, so it is
    // skipped rather than summed.
    assert_eq!(voltage(), None);
    pvt[0].record(
        ChipAddress::One(3),
        Some(Temperature::from_celsius_f64(50.0)),
        Some(Voltage::from_mv(306)),
    );
    assert_eq!(voltage(), Some(Voltage::from_mv(1215)));
}

#[test]
fn a_chain_silent_for_a_second_sample_stops_the_sensor_answering() {
    let (_clock, pvt, stats) = present_board();
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(60.0)),
        None,
    );
    let mut sensor = sensor(pvt, stats);

    // The first silent sample of chain 1 still delivers chain 0's reading.
    sensor.silent_ticks[1] = SILENT_TICKS_BEFORE_FAULT - 1;
    assert!(!sensor.stalled());
    // One more silent sample and the sensor stops answering.
    sensor.silent_ticks[1] = SILENT_TICKS_BEFORE_FAULT;
    assert!(sensor.stalled());
}

#[test]
fn a_sample_submits_the_dies_of_the_chains() {
    let (_clock, pvt, stats) = present_board();
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius(40)),
        None,
    );
    pvt[0].record(
        ChipAddress::One(2),
        Some(Temperature::from_celsius(55)),
        None,
    );
    let mut sensor = sensor(pvt, stats);

    // The channel is shared by every test, so it is emptied first, and
    // it is read here and not drained, so what the sample submits stays.
    while crate::INPUT.try_receive().is_ok() {}
    let mut submitted = || {
        embassy_futures::block_on(sensor.sample());
        let mut submitted = std::vec::Vec::new();
        while let Ok(input) = crate::INPUT.try_receive() {
            submitted.push(input);
        }
        submitted
    };
    let dies = |max, mean| {
        crate::Submitted::Dies(TempStats {
            max: Temperature::from_millicelsius(max),
            mean: Temperature::from_millicelsius(mean),
        })
    };
    // The chips are the driver's to submit, so a sample hands over the
    // dies and nothing else.
    assert_eq!(submitted(), std::vec![dies(55_000, 47_500)]);
    pvt[0].record(
        ChipAddress::One(2),
        Some(Temperature::from_celsius(57)),
        None,
    );
    assert_eq!(submitted(), std::vec![dies(57_000, 48_500)]);
}

#[test]
fn a_stalled_chain_stops_every_chain_submitting() {
    let (_clock, pvt, stats) = present_board();
    let mut sensor = sensor(pvt, stats);
    // The first sample sees both chains turn present, and silence is
    // measured from there. Only chain 0 answers after it.
    let _ = sampled(&mut sensor);
    embassy_time::MockDriver::get().advance(MAX_SILENCE);
    pvt[0].record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius(40)),
        None,
    );

    // The first silent sample of chain 1 still submits the dies of
    // chain 0.
    assert!(sampled(&mut sensor).is_some());
    // The second one stalls the sensor, and chain 0 submits nothing.
    assert_eq!(sampled(&mut sensor), None);
}

#[test]
fn a_sample_records_the_voltage_of_each_complete_chain() {
    let (_clock, pvt, stats) = present_board();
    let die = Some(Temperature::from_celsius(50));
    for chip in ChipAddress::range(TEST_CHIPS) {
        pvt[0].record(chip, die, Some(Voltage::from_mv(300)));
    }
    pvt[1].record(ChipAddress::One(0), die, Some(Voltage::from_mv(310)));
    let mut sensor = sensor(pvt, stats);

    let _ = sampled(&mut sensor);
    assert_eq!(
        stats.readings.voltage(HashboardIndex(0)),
        Some(Voltage::from_mv(1200))
    );
    assert_eq!(
        stats.readings.voltage(HashboardIndex(1)),
        None,
        "one chip is missing"
    );
}

#[test]
fn the_lowest_voltage_is_the_lowest_sum_of_a_chain_that_has_one() {
    let board = Board::new();
    assert_eq!(board.readings.lowest_voltage(), None);

    board
        .readings
        .set_voltage(HashboardIndex(0), Some(Voltage::from_mv(1215)));
    // Chain 1 has no complete sum, so it does not count as the lowest.
    assert_eq!(
        board.readings.lowest_voltage(),
        Some(Voltage::from_mv(1215))
    );

    board
        .readings
        .set_voltage(HashboardIndex(1), Some(Voltage::from_mv(1200)));
    assert_eq!(
        board.readings.lowest_voltage(),
        Some(Voltage::from_mv(1200))
    );
}
