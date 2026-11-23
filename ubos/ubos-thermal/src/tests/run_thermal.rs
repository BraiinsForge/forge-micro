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

//! Integration tests for the halt, release, and re-arm behavior of
//! [`super::run_thermal`], driven against `embassy_time::MockDriver` the
//! same way `ubos-cooling`'s `run_cooling` tests drive that loop.
//!
//! `run_thermal` never returns, so it is polled manually rather than run
//! under an executor: each phase advances the mock clock or scripts the
//! next sensor answer, then polls the task until it parks, so the chain
//! of awaits one tick triggers has settled before the manager traffic or
//! the mask read-back is asserted.

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
use ubos_manager::{Handle as ManagerHandle, Message, PauseReason, PauseSubscription, run_manager};

use super::{
    NoAmbient, NoMcu, Reading, SENSOR_FAULT_TICKS, TempSample, TempStats, TemperatureSensor, clock,
    run_thermal,
};

/// A board sensor whose answers are scripted in advance.
///
/// Panics if `sample` is called more times than the script provides, so
/// a test that drives more ticks than it scripted fails loudly instead of
/// silently reusing the last answer.
struct ScriptedSensor {
    readings: VecDeque<Reading>,
}

impl ScriptedSensor {
    fn new(readings: impl IntoIterator<Item = Reading>) -> Self {
        Self {
            readings: readings.into_iter().collect(),
        }
    }
}

impl TemperatureSensor for ScriptedSensor {
    async fn sample(&mut self) -> Reading {
        self.readings
            .pop_front()
            .expect("BUG: test drove run_thermal past its scripted readings")
    }
}

/// A measured reading of 90 °C, above the `dangerous_hashboard_temp` of the
/// BMM101 that the tests run on. Well above the 45 °C default
/// `cooldown_temp`, so a tick taken while the halt stands evaluates to
/// `Ok` and not to a resume.
fn hot_reading() -> Reading {
    let temp = Temperature::from_millicelsius(90_000);
    Reading::Measured(TempSample {
        chip: None,
        hashboard: Some(TempStats {
            max: temp,
            mean: temp,
        }),
    })
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

/// Advance the mock clock by one sample interval, poll `thermal` until the
/// awaits one tick triggers have settled, and then poll `manager` so that it
/// handles what the tick sent.
fn tick<F: Future, M: Future>(
    driver: &MockDriver,
    thermal: &mut Pin<&mut F>,
    manager: &mut Pin<&mut M>,
) {
    let interval = u64::from(crate::default_config().thermal.sample_interval_ms);
    driver.advance(Duration::from_millis(interval));
    settle(thermal);
    settle(manager);
}

/// Whether the manager published a mask since the last call, and which.
///
/// The manager publishes once for every message it handles, so a tick that
/// sent nothing leaves this `None`.
fn published(subscription: &mut PauseSubscription) -> Option<ubos_manager::PauseReasons> {
    subscription.try_changed()
}

// Each test takes the handles that a board takes in `main`, so it runs in
// a process of its own.
rusty_fork_test! {
    #[test]
    fn unreadable_halts_exactly_once_at_the_budget() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut manager_runner, manager) = ManagerHandle::take();
        let (_mining_runner, mining) = ubos_mining::Handle::take();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut mask = manager.subscribe();
        let mut manager_task = pin!(run_manager(&mut manager_runner, mining));

        // Two ticks past the budget, to prove the halt is not sent again on
        // tick `SENSOR_FAULT_TICKS + 1`.
        let ticks = SENSOR_FAULT_TICKS + 2;
        let sensor = ScriptedSensor::new((0..ticks).map(|_| Reading::Unreadable));

        let mut thermal = pin!(run_thermal(
            sensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            manager,
            config
        ));
        prime(&mut thermal);
        prime(&mut manager_task);

        for tick_number in 1..=ticks {
            tick(driver, &mut thermal, &mut manager_task);
            let sent = published(&mut mask);
            if tick_number == SENSOR_FAULT_TICKS {
                assert!(
                    sent.is_some_and(|mask| mask.contains(PauseReason::TemperatureUnreadable)),
                    "the halt must fire on the exact tick the tolerance budget runs out"
                );
            } else {
                assert!(
                    sent.is_none(),
                    "no message is owed before or after the budget tick, tick {tick_number} sent one"
                );
            }
        }
    }

    #[test]
    fn idle_after_a_halt_releases_it_and_a_following_run_spends_the_full_budget_again() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut manager_runner, manager) = ManagerHandle::take();
        let (_mining_runner, mining) = ubos_mining::Handle::take();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut mask = manager.subscribe();
        let mut manager_task = pin!(run_manager(&mut manager_runner, mining));

        let mut script = std::vec::Vec::new();
        // First run: spend the whole tolerance budget on Unreadable, which
        // halts on the last one.
        for _ in 0..SENSOR_FAULT_TICKS {
            script.push(Reading::Unreadable);
        }
        // The release: a board with nothing powered owes no reading, which
        // must retract the halt even though no measurement ever arrived.
        script.push(Reading::Idle);
        // Second run: Unreadable again. If the tolerance count had not reset
        // on the release, this would halt again on the first tick instead of
        // needing the full budget.
        for _ in 0..SENSOR_FAULT_TICKS {
            script.push(Reading::Unreadable);
        }

        let sensor = ScriptedSensor::new(script);

        let mut thermal = pin!(run_thermal(
            sensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            manager,
            config
        ));
        prime(&mut thermal);
        prime(&mut manager_task);

        // Drive the first run's budget.
        for _ in 0..SENSOR_FAULT_TICKS {
            tick(driver, &mut thermal, &mut manager_task);
        }
        assert!(
            published(&mut mask).is_some_and(|mask| mask.contains(PauseReason::TemperatureUnreadable)),
            "the first run must halt at the end of its budget"
        );
        assert!(
            published(&mut mask).is_none(),
            "the first run must halt exactly once"
        );

        // The idle tick: releases the halt.
        tick(driver, &mut thermal, &mut manager_task);
        assert!(
            published(&mut mask).is_some_and(ubos_manager::PauseReasons::is_empty),
            "an Idle reading after a halt must release it"
        );

        // The second run must not halt again before the budget is spent a
        // second time.
        for tick_number in 1..SENSOR_FAULT_TICKS {
            tick(driver, &mut thermal, &mut manager_task);
            assert!(
                published(&mut mask).is_none(),
                "tick {tick_number} of the second run must not halt before its own budget runs out"
            );
        }
        // The last tick of the second budget must halt again, proving the
        // count reset rather than carrying over remaining tolerance from
        // before the release.
        tick(driver, &mut thermal, &mut manager_task);
        assert!(
            published(&mut mask).is_some_and(|mask| mask.contains(PauseReason::TemperatureUnreadable)),
            "a run after the release must spend the full budget again before halting"
        );
    }

    #[test]
    fn dangerous_temperature_re_arms_after_an_acknowledged_halt_clears_the_mask() {
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut manager_runner, manager) = ManagerHandle::take();
        let (_mining_runner, mining) = ubos_mining::Handle::take();
        let (mut thermal_runner, thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut mask = manager.subscribe();
        let mut manager_task = pin!(run_manager(&mut manager_runner, mining));

        // Three over-threshold samples: trip, stand, and re-arm.
        let sensor = ScriptedSensor::new([hot_reading(), hot_reading(), hot_reading()]);

        let mut thermal = pin!(run_thermal(
            sensor,
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            manager,
            config
        ));
        prime(&mut thermal);
        prime(&mut manager_task);

        // Tick 1: an over-threshold sample with the mask clear must trip the
        // halt.
        tick(driver, &mut thermal, &mut manager_task);
        assert!(
            published(&mut mask).is_some_and(|mask| mask.contains(PauseReason::DangerousTemperature)),
            "a first over-threshold sample must trip the halt"
        );
        assert!(
            thermal_handle.current().is_some(),
            "the loop must publish through the handle it was given"
        );

        // Tick 2: the halt stands in the mask the manager published, so a
        // sample still above the threshold and far above `cooldown_temp`
        // evaluates to `Ok` and asks for nothing. This is the half that
        // proves the loop reads the mask at all: with no halt state anywhere,
        // this tick would trip a second time.
        tick(driver, &mut thermal, &mut manager_task);
        assert!(
            published(&mut mask).is_none(),
            "a sample taken while the halt stands must not ask for it twice"
        );

        // The operator acknowledges, which is the only way out of this halt
        // on a board whose chips are its thermometer, see `docs/MINING.md`
        // under "Two ways out of an overheat halt". The API sends it to the
        // manager, which clears the bit and publishes.
        {
            // One poll, because the send completes on it and a finished
            // future must not be polled again.
            let mut acknowledge = pin!(manager.send(Message::TemperatureAcknowledged));
            let _ = acknowledge
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
        }
        settle(&mut manager_task);
        assert!(
            !manager.holds(PauseReason::DangerousTemperature),
            "the acknowledgement must clear the halt"
        );
        let _ = published(&mut mask);

        // Tick 3: the same over-threshold sample must trip the halt again.
        // This is the half that proves the read is fresh. A flag of its own set
        // on tick 1 and never told about the acknowledgement would still
        // read as held, and `evaluate` would answer `Ok` at any temperature
        // for the rest of the loop's life.
        tick(driver, &mut thermal, &mut manager_task);
        assert!(
            published(&mut mask).is_some_and(|mask| mask.contains(PauseReason::DangerousTemperature)),
            "the trip must re-arm once the manager's mask reports the halt cleared"
        );
    }
}

/// A sensor that submits more readings than the input channel holds
/// while it samples, and sets `done` once it has submitted them all.
struct SubmittingSensor {
    done: &'static portable_atomic::AtomicBool,
}

impl TemperatureSensor for SubmittingSensor {
    async fn sample(&mut self) -> Reading {
        for _ in 0..2 * super::INPUT_DEPTH {
            let source = ubos_temperature::TempSource::Ambient;
            let value = Temperature::from_millicelsius(25_000);
            crate::Handle(())
                .submit(ubos_temperature::Reading { source, value })
                .await;
        }
        self.done.store(true, portable_atomic::Ordering::SeqCst);
        Reading::Idle
    }
}

rusty_fork_test! {
    #[test]
    fn a_source_that_submits_more_than_the_channel_holds_is_drained() {
        static DONE: portable_atomic::AtomicBool = portable_atomic::AtomicBool::new(false);
        let _clock = clock();
        let driver = MockDriver::get();
        driver.reset();
        let (mut manager_runner, manager) = ManagerHandle::take();
        let (_mining_runner, mining) = ubos_mining::Handle::take();
        let (mut thermal_runner, _thermal_handle) = crate::Handle::take();
        let (mut config_runner, config) = ubos_config::Handle::take();
        let store = StoreDir::new();
        let mut config_task = pin!(run_config(&store, &mut config_runner));
        settle(&mut config_task);
        let mut manager_task = pin!(run_manager(&mut manager_runner, mining));
        let mut thermal = pin!(run_thermal(
            SubmittingSensor { done: &DONE },
            NoAmbient,
            NoMcu,
            &mut thermal_runner,
            manager,
            config
        ));
        prime(&mut thermal);
        prime(&mut manager_task);
        tick(driver, &mut thermal, &mut manager_task);
        assert!(DONE.load(portable_atomic::Ordering::SeqCst));
        assert!(
            super::INPUT.try_receive().is_err(),
            "nothing is left queued"
        );
    }
}
