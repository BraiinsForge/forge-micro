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

//! Integration tests for [`super::run`], driven against
//! `embassy_time::MockDriver` the same way `ubos-cooling`'s `run_cooling`
//! tests drive that loop.
//!
//! `run` never returns, so it is polled manually rather than run
//! under an executor: each phase ends a sample as the sampling task does,
//! then polls the task until it parks, so the chain of awaits one tick
//! triggers has settled before the published values are asserted.

// The crate is `#![no_std]`; the host test binary links std for the
// sensor script queue.
extern crate std;

use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Waker};

use embassy_time::{Duration, MockDriver};
use rusty_fork::rusty_fork_test;
use ubos_config::{
    DefaultTuning, FileStorage, MiningBounds, MiningInfo, Runner as ConfigRunner, TunerInfo,
};
use ubos_core::units::Temperature;

use ubos_temperature::{
    Handle as Readings, Reading, ReadingSource, Runner as SamplingRunner, TempSource,
};

use super::{clock, run};

/// The directory of the config store of one test, removed when the test
/// ends.
struct StoreDir(std::path::PathBuf);

impl StoreDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(std::format!(
            "ubos_thermal_{}_{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
        )))
    }
}

impl Drop for StoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The bounds of a machine that offers the fixed tuner alone.
static BOUNDS: MiningBounds =
    ubos_asic::BMM101_HASHBOARD.bounds(&[TunerInfo::Fixed { name: "Fixed" }], None);

/// The mining metadata the config task runs on. The defaults it publishes
/// are those of [`crate::default_config`].
static INFO: MiningInfo = ubos_asic::BMM101_HASHBOARD.mining_info(&BOUNDS, DefaultTuning::Fixed);

/// The real config task over an empty store, which publishes the defaults
/// on its first poll. `run` waits for that publish before it reads
/// its config.
fn run_config<'a>(
    store: &StoreDir,
    runner: &'a mut ConfigRunner,
) -> impl Future<Output = core::convert::Infallible> + use<'a> {
    let dir = store.0.clone();
    async move {
        ubos_config::run(
            runner,
            FileStorage::new(dir),
            &ubos_asic::BMM101_HASHBOARD.defaults,
            &INFO,
            None,
            None,
        )
        .await
    }
}

/// Polls per phase. One tick's chain of awaits is a handful of polls, so
/// this covers it with room to spare. Polling a task that has already
/// parked returns `Pending` and changes nothing, so a fixed count needs
/// no test for whether the task is still making progress.
const POLL_BUDGET: usize = 16;

/// Poll `thermal` far enough for the awaits one phase triggers to settle.
fn settle<F: Future>(thermal: &mut Pin<&mut F>) {
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..POLL_BUDGET {
        let _ = thermal.as_mut().poll(&mut cx);
    }
}

/// Poll `thermal` far enough to take a backlog of twice the depth of the
/// input channel. The loop yields after each input, so an input costs a
/// poll, and one that waited on the full channel costs a second.
fn settle_backlog<F: Future>(thermal: &mut Pin<&mut F>) {
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..4 * super::INPUT_DEPTH {
        let _ = thermal.as_mut().poll(&mut cx);
    }
}

/// Poll `task` to its first park, with no clock advance.
fn prime<F: Future>(task: &mut Pin<&mut F>) {
    settle(task);
}

/// End a sample as the sampling task does and poll `thermal` until the
/// awaits one tick triggers have settled.
fn tick<F: Future>(sampling: &mut SamplingRunner, thermal: &mut Pin<&mut F>) {
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        pin!(sampling.end_sample()).poll(&mut cx).is_ready(),
        "the channel of readings is full"
    );
    settle(thermal);
}

/// The sample interval of [`crate::default_config`].
fn interval() -> Duration {
    Duration::from_millis(u64::from(
        crate::default_config().thermal.sample_interval_ms,
    ))
}

/// Hand `reading` to thermal as a source does. The channel has room in
/// every test, so the submit is ready on its first poll.
fn submit(readings: Readings, reading: Reading) {
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        pin!(readings.submit(reading)).poll(&mut cx).is_ready(),
        "the channel of readings is full"
    );
}

fn chip_reading(celsius: i32) -> Reading {
    let chain = ubos_core::index::HashboardIndex(0);
    Reading {
        source: TempSource::chip(chain, ubos_core::index::ChipAddress::One(0)),
        value: Temperature::from_celsius(celsius),
    }
}

fn ambient_reading(celsius: i32) -> Reading {
    Reading {
        source: TempSource::Ambient,
        value: Temperature::from_celsius(celsius),
    }
}

rusty_fork_test! {
    #[test]
    fn a_backlog_of_readings_is_taken_between_two_ticks() {
        let _clock = clock();
        MockDriver::get().reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        for _ in 0..32 {
            submit(readings, ambient_reading(25));
        }
        settle_backlog(&mut thermal);
        // One settle is shorter than the backlog, so the end of the
        // sample is reached only when nothing is left queued before it.
        tick(&mut sampling, &mut thermal);
        assert_eq!(thermal_handle.ambient(), Some(Temperature::from_celsius(25)));
    }

    #[test]
    fn a_submitted_reading_reaches_the_watches_on_the_tick() {
        let _clock = clock();
        MockDriver::get().reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        submit(readings, chip_reading(70));
        tick(&mut sampling, &mut thermal);
        let watches = &super::WATCHES;
        let seventy = Temperature::from_celsius(70);
        let sample = watches.filtered.try_get().flatten();
        assert_eq!(
            sample.and_then(|sample| sample.chip).map(|chip| chip.max),
            Some(seventy)
        );
        // The default target follows the hashboard, which read nothing.
        assert_eq!(watches.pid.try_get().map(|pid| pid.temp), Some(None));
        assert_eq!(watches.ambient.try_get(), Some(None));
    }

    #[test]
    fn the_ambient_reading_reaches_its_watch_on_the_tick() {
        let _clock = clock();
        MockDriver::get().reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        assert_eq!(thermal_handle.ambient(), None);
        submit(readings, ambient_reading(25));
        tick(&mut sampling, &mut thermal);
        assert_eq!(thermal_handle.ambient(), Some(Temperature::from_celsius(25)));
    }

    #[test]
    fn a_slow_sample_keeps_its_readings_and_those_of_the_two_before() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        let chip = || thermal_handle.filtered().and_then(|sample| sample.chip).map(|chip| chip.max);
        // Each sample takes ten intervals, which is longer than any
        // window that reads the clock.
        let slow = interval() * 10;
        submit(readings, chip_reading(70));
        driver.advance(slow);
        tick(&mut sampling, &mut thermal);
        let seventy = Some(Temperature::from_celsius(70));
        assert_eq!(chip(), seventy, "the sample that took the reading");
        for _ in 0..2 {
            driver.advance(slow);
            tick(&mut sampling, &mut thermal);
            assert_eq!(chip(), seventy, "a sample that missed it");
        }
        driver.advance(slow);
        tick(&mut sampling, &mut thermal);
        assert_eq!(chip(), None, "the third sample that missed it");
    }

    #[test]
    fn a_chip_that_a_driver_submits_is_taken_while_the_loop_waits() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (_sampling, mut receiver, _readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        // A driver submits more chips than the channel holds and never
        // waits: the ones past the depth are dropped.
        let chain = ubos_core::index::HashboardIndex(0);
        let chip = ubos_core::index::ChipAddress::One(0);
        for _ in 0..2 * super::INPUT_DEPTH {
            thermal_handle.submit_chip(chain, chip, Temperature::from_celsius(70));
        }
        assert!(super::INPUT.is_full());
        settle_backlog(&mut thermal);
        assert!(
            super::INPUT.try_receive().is_err(),
            "the loop takes the chips between two ticks"
        );
    }

    #[test]
    fn a_chip_at_its_limit_sets_the_flag_and_the_tick_still_publishes() {
        let _clock = clock();
        MockDriver::get().reset();
        let (mut sampling, mut receiver, _readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        let mut pid = thermal_handle.subscribe_pid_temperature();
        prime(&mut thermal);
        // The default config judges no chip, so the test gives it a limit.
        let limit = Temperature::from_celsius(90);
        let mut judged = crate::default_config();
        judged.thermal.dangerous.chip = Some(limit);
        assert!(config.try_send(ubos_config::Message::Update(judged)));
        settle(&mut config_task);
        tick(&mut sampling, &mut thermal);
        thermal_handle.submit_chip(
            ubos_core::index::HashboardIndex(0),
            ubos_core::index::ChipAddress::One(0),
            limit,
        );
        settle(&mut thermal);
        assert!(thermal_handle.is_dangerous());
        let _ = pid.try_changed();
        for _ in 0..2 {
            tick(&mut sampling, &mut thermal);
            assert!(pid.try_changed().is_some(), "a tick sends the PID watch");
        }
    }
}

/// The I2C address of the sensor that the hashboard readings name.
const SENSOR: u8 = 0x48;

fn hashboard_reading(celsius: i32) -> Reading {
    let chain = ubos_core::index::HashboardIndex(0);
    Reading {
        source: TempSource::hashboard(chain, SENSOR),
        value: Temperature::from_celsius(celsius),
    }
}

/// Cool ticks that the wake test runs, which is more than the hot pane
/// takes to leave the window of the filter.
const COOL_TICKS: usize = 6;

rusty_fork_test! {
    #[test]
    fn a_hot_reading_sets_the_flag_and_an_acknowledge_clears_it() {
        // Below the limit and above the cooldown, so only an acknowledge
        // clears the flag.
        const WARM: i32 = 60;
        let _clock = clock();
        MockDriver::get().reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        assert!(!thermal_handle.is_dangerous(), "the flag starts clear");
        submit(readings, hashboard_reading(200));
        tick(&mut sampling, &mut thermal);
        assert!(
            thermal_handle.is_dangerous(),
            "a reading above its limit sets the flag"
        );
        for _ in 0..COOL_TICKS {
            submit(readings, hashboard_reading(WARM));
            tick(&mut sampling, &mut thermal);
        }
        assert!(thermal_handle.is_dangerous(), "the readings did not cool");
        thermal_handle.acknowledge();
        submit(readings, hashboard_reading(WARM));
        tick(&mut sampling, &mut thermal);
        assert!(
            !thermal_handle.is_dangerous(),
            "the acknowledge clears the flag"
        );
    }

    #[test]
    fn an_acknowledge_of_a_board_that_is_still_hot_keeps_the_flag() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        submit(readings, hashboard_reading(200));
        tick(&mut sampling, &mut thermal);
        assert!(thermal_handle.is_dangerous());
        // The acknowledge arrives between a tick and the next hot reading.
        thermal_handle.acknowledge();
        driver.advance(interval() * 2);
        settle(&mut thermal);
        assert!(thermal_handle.is_dangerous(), "the clock ends no sample");
        submit(readings, hashboard_reading(200));
        tick(&mut sampling, &mut thermal);
        assert!(thermal_handle.is_dangerous(), "the hot reading drops it");
    }

    #[test]
    fn a_subscriber_wakes_once_when_a_reading_crosses_and_once_when_it_cools() {
        let _clock = clock();
        MockDriver::get().reset();
        let (mut sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        let mut flag = thermal_handle.subscribe_dangerous();
        assert_eq!(flag.try_changed(), Some(false), "the first publish");
        submit(readings, hashboard_reading(200));
        tick(&mut sampling, &mut thermal);
        assert_eq!(flag.try_changed(), Some(true), "the reading crossed");
        submit(readings, hashboard_reading(200));
        tick(&mut sampling, &mut thermal);
        assert_eq!(flag.try_changed(), None, "a second hot tick is no edge");
        let wakes: std::vec::Vec<bool> = (0..COOL_TICKS)
            .filter_map(|_| {
                submit(readings, hashboard_reading(30));
                tick(&mut sampling, &mut thermal);
                flag.try_changed()
            })
            .collect();
        assert_eq!(wakes, [false], "the readings cooled");
    }

    #[test]
    fn a_lowered_limit_judges_the_next_reading() {
        // Below the default limit of the hashboard and above the lowered
        // one.
        const WARM: i32 = 55;
        const LOWERED: i32 = 50;
        let _clock = clock();
        MockDriver::get().reset();
        let (_sampling, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut thermal);
        submit(readings, hashboard_reading(WARM));
        settle(&mut thermal);
        assert!(!thermal_handle.is_dangerous(), "the default limit is higher");
        let mut lowered = crate::default_config();
        lowered.thermal.dangerous.hashboard = Some(Temperature::from_celsius(LOWERED));
        assert!(config.try_send(ubos_config::Message::Update(lowered)));
        settle(&mut config_task);
        settle(&mut thermal);
        submit(readings, hashboard_reading(WARM));
        settle(&mut thermal);
        assert!(
            thermal_handle.is_dangerous(),
            "the new limit needs no end of a sample"
        );
    }
}

/// A source that counts its samples and submits one chip reading of 70 °C
/// on each.
struct CountedChip {
    readings: Readings,
    samples: &'static portable_atomic::AtomicU8,
}

impl ReadingSource for CountedChip {
    async fn sample(&mut self) {
        self.samples.fetch_add(1, portable_atomic::Ordering::SeqCst);
        self.readings.submit(chip_reading(70)).await;
    }
}

rusty_fork_test! {
    /// The sampling task of `ubos-temperature` and this loop, side by side
    /// as two tasks of a firmware.
    #[test]
    fn a_sampled_source_reaches_the_watches_and_a_new_interval_restarts_the_sampling() {
        static SAMPLES: portable_atomic::AtomicU8 = portable_atomic::AtomicU8::new(0);
        let samples = || SAMPLES.load(portable_atomic::Ordering::SeqCst);
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut temperature_runner, mut receiver, readings) = Readings::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let source = CountedChip { readings, samples: &SAMPLES };
        let mut sampling = pin!(ubos_temperature::run(source, &mut temperature_runner, config));
        let mut thermal = pin!(run(&mut receiver, &mut thermal_runner, config));
        prime(&mut sampling);
        prime(&mut thermal);
        assert_eq!(samples(), 1, "the start takes a sample");
        let both = |sampling: &mut Pin<&mut _>, thermal: &mut Pin<&mut _>| {
            settle(sampling);
            settle(thermal);
        };
        driver.advance(interval());
        both(&mut sampling, &mut thermal);
        let seventy = Some(Temperature::from_celsius(70));
        let chip = || thermal_handle.filtered().and_then(|sample| sample.chip).map(|chip| chip.max);
        assert_eq!((samples(), chip()), (2, seventy));

        let mut doubled = crate::default_config();
        doubled.thermal.sample_interval_ms *= 2;
        let interval = u64::from(doubled.thermal.sample_interval_ms);
        assert!(config.try_send(ubos_config::Message::Update(doubled)));
        settle(&mut config_task);
        both(&mut sampling, &mut thermal);
        assert_eq!(samples(), 3, "the restart takes a sample");

        driver.advance(Duration::from_millis(interval / 2));
        both(&mut sampling, &mut thermal);
        assert_eq!(samples(), 3, "the old interval ends no wait");
        driver.advance(Duration::from_millis(interval / 2));
        both(&mut sampling, &mut thermal);
        assert_eq!(samples(), 4, "the new interval does");
        assert_eq!(chip(), seventy, "the first tick after the restart has a reading");
    }
}

/// A source that counts its samples and takes `first` for the first of
/// them.
struct SlowFirst {
    first: Duration,
    samples: &'static portable_atomic::AtomicU8,
}

impl ReadingSource for SlowFirst {
    async fn sample(&mut self) {
        if self.samples.fetch_add(1, portable_atomic::Ordering::SeqCst) == 0 {
            embassy_time::Timer::after(self.first).await;
        }
    }
}

rusty_fork_test! {
    #[test]
    fn a_sample_that_overran_its_interval_is_followed_by_one_sample_an_interval_later() {
        static SAMPLES: portable_atomic::AtomicU8 = portable_atomic::AtomicU8::new(0);
        let samples = || SAMPLES.load(portable_atomic::Ordering::SeqCst);
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut temperature_runner, _receiver, _readings) = Readings::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        // The first sample passes three deadlines.
        let source = SlowFirst { first: interval() * 7 / 2, samples: &SAMPLES };
        let mut sampling = pin!(ubos_temperature::run(source, &mut temperature_runner, config));
        prime(&mut sampling);
        driver.advance(interval() * 7 / 2);
        settle(&mut sampling);
        assert_eq!(samples(), 1, "no missed deadline takes a sample");
        driver.advance(interval());
        settle(&mut sampling);
        assert_eq!(samples(), 2, "the next sample is one interval later");
    }

    #[test]
    fn a_late_wake_is_followed_by_one_sample_an_interval_later() {
        static SAMPLES: portable_atomic::AtomicU8 = portable_atomic::AtomicU8::new(0);
        let samples = || SAMPLES.load(portable_atomic::Ordering::SeqCst);
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut temperature_runner, _receiver, readings) = Readings::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let source = CountedChip { readings, samples: &SAMPLES };
        let mut sampling = pin!(ubos_temperature::run(source, &mut temperature_runner, config));
        prime(&mut sampling);
        // Nothing polls the task while three of its deadlines pass.
        driver.advance(interval() * 7 / 2);
        settle(&mut sampling);
        assert_eq!(samples(), 2, "no missed deadline takes a sample");
        driver.advance(interval());
        settle(&mut sampling);
        assert_eq!(samples(), 3, "the next sample is one interval later");
    }
}
