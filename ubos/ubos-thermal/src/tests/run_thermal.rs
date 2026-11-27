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

//! Integration tests for [`super::run_thermal`], driven against
//! `embassy_time::MockDriver` the same way `ubos-cooling`'s `run_cooling`
//! tests drive that loop.
//!
//! `run_thermal` never returns, so it is polled manually rather than run
//! under an executor: each phase advances the mock clock, then polls the
//! task until it parks, so the chain of awaits one tick triggers has
//! settled before the published values are asserted.

// The crate is `#![no_std]`; the host test binary links std for the
// sensor script queue.
extern crate std;

use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Waker};
use std::collections::VecDeque;

use embassy_time::{Duration, MockDriver};
use rusty_fork::rusty_fork_test;
use ubos_config::{
    DefaultTuning, FileStorage, MiningBounds, MiningInfo, Runner as ConfigRunner, TunerInfo,
};
use ubos_core::units::Temperature;

use super::{AmbientSensor, NoAmbient, NoMcu, TemperatureSensor, clock, run_thermal};

/// A sensor that reads nothing and submits nothing.
struct IdleSensor;

impl TemperatureSensor for IdleSensor {
    async fn sample(&mut self) {}
}

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
/// on its first poll. `run_thermal` waits for that publish before it reads
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

/// Poll `thermal` to its first park, with no clock advance.
///
/// `run_thermal`'s setup — reading the config, building the `Ticker` —
/// runs synchronously before its first await, which is the `Ticker`
/// itself; nothing in that setup is on the clock this test controls.
/// Call this once, before the first [`tick`], so that call is the first
/// one to actually fire the ticker rather than spending its clock
/// advance on construction.
fn prime<F: Future>(thermal: &mut Pin<&mut F>) {
    settle(thermal);
}

/// Advance the mock clock by one sample interval and poll `thermal` until
/// the awaits one tick triggers have settled.
fn tick<F: Future>(driver: &MockDriver, thermal: &mut Pin<&mut F>) {
    let interval = u64::from(crate::default_config().thermal.sample_interval_ms);
    driver.advance(Duration::from_millis(interval));
    settle(thermal);
}

/// A sensor that submits more readings than the input channel holds
/// while it samples, and sets `done` once it has submitted them all.
struct SubmittingSensor {
    done: &'static portable_atomic::AtomicBool,
}

impl TemperatureSensor for SubmittingSensor {
    async fn sample(&mut self) {
        for _ in 0..2 * super::INPUT_DEPTH {
            let source = ubos_temperature::TempSource::Ambient;
            let value = Temperature::from_millicelsius(25_000);
            crate::Handle(())
                .submit(ubos_temperature::Reading { source, value })
                .await;
        }
        self.done.store(true, portable_atomic::Ordering::SeqCst);
    }
}

rusty_fork_test! {
    #[test]
    fn a_source_that_submits_more_than_the_channel_holds_is_drained() {
        static DONE: portable_atomic::AtomicBool = portable_atomic::AtomicBool::new(false);
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            SubmittingSensor { done: &DONE },
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        tick(driver, &mut thermal);
        settle_backlog(&mut thermal);
        assert!(DONE.load(portable_atomic::Ordering::SeqCst));
        assert!(
            super::INPUT.try_receive().is_err(),
            "nothing is left queued"
        );
    }
}

/// Sample intervals that [`SlowSensor`] waits for after it submits, which
/// is longer than the window of a filter.
const SLOW_SAMPLE_INTERVALS: u32 = 4;

/// A sensor that submits one chip reading of 70 °C and then takes longer
/// than the window of a filter to end its sample.
struct SlowSensor;

impl TemperatureSensor for SlowSensor {
    async fn sample(&mut self) {
        ChipSensor.sample().await;
        let interval = u64::from(crate::default_config().thermal.sample_interval_ms);
        embassy_time::Timer::after(Duration::from_millis(interval) * SLOW_SAMPLE_INTERVALS).await;
    }
}

rusty_fork_test! {
    #[test]
    fn a_reading_of_a_slow_sample_is_published_on_its_tick() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            SlowSensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        tick(driver, &mut thermal);
        let published = || super::WATCHES.filtered.try_get().flatten();
        assert_eq!(published(), None, "the sample has not ended");
        for _ in 0..SLOW_SAMPLE_INTERVALS {
            tick(driver, &mut thermal);
        }
        let chip = published().and_then(|sample| sample.chip);
        assert_eq!(chip.map(|chip| chip.max), Some(Temperature::from_celsius(70)));
    }
}

/// A sensor that submits one chip reading of 70 °C while it samples.
struct ChipSensor;

impl TemperatureSensor for ChipSensor {
    async fn sample(&mut self) {
        let chain = ubos_core::index::HashboardIndex(0);
        let source =
            ubos_temperature::TempSource::chip(chain, ubos_core::index::ChipAddress::One(0));
        let value = Temperature::from_celsius(70);
        crate::Handle(())
            .submit(ubos_temperature::Reading { source, value })
            .await;
    }
}

rusty_fork_test! {
    #[test]
    fn a_submitted_reading_reaches_the_watches_on_the_tick() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            ChipSensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        tick(driver, &mut thermal);
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
}

/// An ambient sensor that reads 25 °C on every tick.
struct WarmAir;

impl AmbientSensor for WarmAir {
    async fn ambient(&mut self) -> Option<Temperature> {
        Some(Temperature::from_celsius(25))
    }
}

rusty_fork_test! {
    #[test]
    fn the_ambient_reading_reaches_its_watch_on_its_own_tick() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            ChipSensor,
            WarmAir,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        assert_eq!(thermal_handle.ambient(), None);
        tick(driver, &mut thermal);
        assert_eq!(thermal_handle.ambient(), Some(Temperature::from_celsius(25)));
    }
}

rusty_fork_test! {
    #[test]
    fn a_chip_that_a_driver_submits_is_taken_while_the_loop_waits() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        // The script is empty, so a tick that fired would panic.
        let mut thermal = pin!(run_thermal(
            IdleSensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            config
        ));
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
}

/// An ambient sensor that never answers, so the loop stays in its read.
struct SilentAir;

impl AmbientSensor for SilentAir {
    async fn ambient(&mut self) -> Option<Temperature> {
        core::future::pending().await
    }
}

rusty_fork_test! {
    #[test]
    fn a_chip_that_a_driver_submits_is_taken_while_the_ambient_sensor_reads() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            IdleSensor,
            SilentAir,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        // The tick ends its sample and parks in the ambient read.
        tick(driver, &mut thermal);
        let chain = ubos_core::index::HashboardIndex(0);
        let chip = ubos_core::index::ChipAddress::One(0);
        for _ in 0..2 * super::INPUT_DEPTH {
            thermal_handle.submit_chip(chain, chip, Temperature::from_celsius(70));
        }
        assert!(super::INPUT.is_full());
        settle_backlog(&mut thermal);
        assert!(
            super::INPUT.try_receive().is_err(),
            "the loop takes the chips while the ambient sensor reads"
        );
    }
}

/// The I2C address of the sensor that the hashboard readings name.
const SENSOR: u8 = 0x48;

/// A sensor that submits one hashboard reading for each queued temperature,
/// in degrees, one on each sample.
struct ScriptedHashboard {
    degrees: VecDeque<i32>,
}

impl TemperatureSensor for ScriptedHashboard {
    async fn sample(&mut self) {
        let celsius = self.degrees.pop_front().expect("BUG: past the script");
        let chain = ubos_core::index::HashboardIndex(0);
        let source = ubos_temperature::TempSource::hashboard(chain, SENSOR);
        let value = Temperature::from_celsius(celsius);
        crate::Handle(())
            .submit(ubos_temperature::Reading { source, value })
            .await;
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
        let driver = MockDriver::get();
        driver.reset();
        let sensor = ScriptedHashboard {
            degrees: core::iter::once(200)
                .chain(core::iter::repeat_n(WARM, COOL_TICKS + 1))
                .collect(),
        };
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            sensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        assert!(!thermal_handle.is_dangerous(), "the flag starts clear");
        tick(driver, &mut thermal);
        assert!(
            thermal_handle.is_dangerous(),
            "a reading above its limit sets the flag"
        );
        for _ in 0..COOL_TICKS {
            tick(driver, &mut thermal);
        }
        assert!(thermal_handle.is_dangerous(), "the readings did not cool");
        thermal_handle.acknowledge();
        tick(driver, &mut thermal);
        assert!(
            !thermal_handle.is_dangerous(),
            "the acknowledge clears the flag"
        );
    }

    #[test]
    fn a_subscriber_wakes_once_when_a_reading_crosses_and_once_when_it_cools() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let sensor = ScriptedHashboard {
            degrees: [200, 200]
                .into_iter()
                .chain(core::iter::repeat_n(30, COOL_TICKS))
                .collect(),
        };
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut thermal = pin!(run_thermal(
            sensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            config
        ));
        prime(&mut thermal);
        let mut flag = thermal_handle.subscribe_dangerous();
        assert_eq!(flag.try_changed(), Some(false), "the first publish");
        tick(driver, &mut thermal);
        assert_eq!(flag.try_changed(), Some(true), "the reading crossed");
        tick(driver, &mut thermal);
        assert_eq!(flag.try_changed(), None, "a second hot tick is no edge");
        let wakes: std::vec::Vec<bool> = (0..COOL_TICKS)
            .filter_map(|_| {
                tick(driver, &mut thermal);
                flag.try_changed()
            })
            .collect();
        assert_eq!(wakes, [false], "the readings cooled");
    }
}
