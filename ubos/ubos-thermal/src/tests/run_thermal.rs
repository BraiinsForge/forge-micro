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
use ubos_core::units::Temperature;
use ubos_manager::{
    CommandChannel, Handle as ManagerHandle, Message, PauseReason, PauseReasons, PauseWatch,
};

use super::{
    NoAmbient, Reading, SENSOR_FAULT_TICKS, Sample, TemperatureSensor, clock, run_thermal,
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

/// A measured reading at the default `dangerous_estimated_temp_c` (90 °C), which
/// `evaluate` trips inclusively. Well above the 45 °C default
/// `cooldown_temp_c`, so a tick taken while the halt stands evaluates to
/// `Ok` and not to a resume.
fn hot_reading() -> Reading {
    Reading::Measured(Sample::sole(Temperature::from_millicelsius(90_000)))
}

/// Drain every `Message` queued so far on the manager's command channel
/// and apply each to `mask`, then publish it, as `run_manager` would.
/// The publish is what the loop's `holds` read-back sees on the next
/// tick, so a test that drained without publishing would leave the loop
/// judging every later tick against a mask its own messages never
/// reached.
pub(crate) fn drain_messages(
    manager: &ManagerChannels,
    mask: &mut PauseReasons,
) -> std::vec::Vec<Message> {
    let mut messages = std::vec::Vec::new();
    while let Ok(message) = manager.commands.try_receive() {
        *mask = mask.apply(message);
        messages.push(message);
    }
    manager.state.sender().send(*mask);
    messages
}

/// The manager channels one test drives, standing in for `run_manager`.
///
/// Each test declares its own, so no test sees another's queued messages
/// or published mask. A single set shared by a helper that more than one
/// test calls would put them back on one channel pair.
pub(crate) struct ManagerChannels {
    pub(crate) state: PauseWatch,
    pub(crate) commands: CommandChannel,
}

impl ManagerChannels {
    pub(crate) const fn new() -> Self {
        Self {
            state: PauseWatch::new(),
            commands: CommandChannel::new(),
        }
    }

    /// The handle `run_thermal` takes, reading and writing these
    /// channels rather than the ones the firmware runs on.
    pub(crate) fn handle(&'static self) -> ManagerHandle {
        ManagerHandle::new(&self.state, &self.commands)
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

/// Advance the mock clock by one sample interval and poll `thermal`
/// until the awaits one tick triggers — the sensor sample, the manager
/// send, the watch publish — have settled.
fn tick<F: Future>(driver: &MockDriver, thermal: &mut Pin<&mut F>) {
    let interval = u64::from(ubos_config::Config::default().thermal.sample_interval_ms);
    driver.advance(Duration::from_millis(interval));
    settle(thermal);
}

#[test]
fn unreadable_halts_exactly_once_at_the_budget() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let _clock = clock();
    let driver = MockDriver::get();
    driver.reset();
    let mut mask = PauseReasons::default();
    let manager = MANAGER.handle();
    drain_messages(&MANAGER, &mut mask);

    // Two ticks past the budget, to prove the halt is not sent again on
    // tick `SENSOR_FAULT_TICKS + 1`.
    let ticks = SENSOR_FAULT_TICKS + 2;
    let sensor = ScriptedSensor::new((0..ticks).map(|_| Reading::Unreadable));

    let mut thermal = pin!(run_thermal(
        sensor,
        NoAmbient,
        manager,
        ubos_config::handle()
    ));
    prime(&mut thermal);

    for tick_number in 1..=ticks {
        tick(driver, &mut thermal);
        let sent = drain_messages(&MANAGER, &mut mask);
        if tick_number == SENSOR_FAULT_TICKS {
            assert_eq!(
                sent,
                std::vec![Message::TemperatureUnreadable],
                "the halt must fire on the exact tick the tolerance budget runs out"
            );
        } else {
            assert!(
                sent.is_empty(),
                "no message is owed before or after the budget tick, tick {tick_number} sent {}",
                sent.len()
            );
        }
    }
}

#[test]
fn idle_after_a_halt_releases_it_and_a_following_run_spends_the_full_budget_again() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let _clock = clock();
    let driver = MockDriver::get();
    driver.reset();
    let mut mask = PauseReasons::default();
    let manager = MANAGER.handle();
    drain_messages(&MANAGER, &mut mask);

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
        manager,
        ubos_config::handle()
    ));
    prime(&mut thermal);

    // Drive the first run's budget.
    for _ in 0..SENSOR_FAULT_TICKS {
        tick(driver, &mut thermal);
    }
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::TemperatureUnreadable],
        "the first run must halt exactly once, at the end of its budget"
    );

    // The idle tick: releases the halt.
    tick(driver, &mut thermal);
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::TemperatureReadable],
        "an Idle reading after a halt must release it"
    );

    // The second run must not halt again before the budget is spent a
    // second time.
    for tick_number in 1..SENSOR_FAULT_TICKS {
        tick(driver, &mut thermal);
        assert!(
            drain_messages(&MANAGER, &mut mask).is_empty(),
            "tick {tick_number} of the second run must not halt before its own budget runs out"
        );
    }
    // The last tick of the second budget must halt again, proving the
    // count reset rather than carrying over remaining tolerance from
    // before the release.
    tick(driver, &mut thermal);
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::TemperatureUnreadable],
        "a run after the release must spend the full budget again before halting"
    );
}

#[test]
fn dangerous_temperature_re_arms_after_an_acknowledged_halt_clears_the_mask() {
    static MANAGER: ManagerChannels = ManagerChannels::new();
    let _clock = clock();
    let driver = MockDriver::get();
    driver.reset();
    let mut mask = PauseReasons::default();
    let manager = MANAGER.handle();
    drain_messages(&MANAGER, &mut mask);

    // Every tick is over the threshold. What changes between them is the
    // mask, which is the only thing this loop may read the halt state
    // from.
    let sensor = ScriptedSensor::new([hot_reading(), hot_reading(), hot_reading()]);

    let mut thermal = pin!(run_thermal(
        sensor,
        NoAmbient,
        manager,
        ubos_config::handle()
    ));
    prime(&mut thermal);

    // Tick 1: an over-threshold sample with the mask clear must trip the
    // halt.
    tick(driver, &mut thermal);
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::DangerousTemperature],
        "a first over-threshold sample must trip the halt"
    );

    // Tick 2: the halt stands in the mask the drain published, so a
    // sample still above the threshold and far above `cooldown_temp_c`
    // evaluates to `Ok` and asks for nothing. This is the half that
    // proves the loop reads the mask at all: with no halt state anywhere,
    // this tick would trip a second time.
    tick(driver, &mut thermal);
    assert!(
        drain_messages(&MANAGER, &mut mask).is_empty(),
        "a sample taken while the halt stands must not ask for it twice"
    );

    // The operator acknowledges, which is the only way out of this halt
    // on a board whose chips are its thermometer, see `docs/MINING.md`
    // under "Two ways out of an overheat halt". The API sends it to the
    // manager, which clears the bit and publishes; here the drain stands
    // in for both.
    mask = mask.apply(Message::TemperatureAcknowledged);
    MANAGER.state.sender().send(mask);
    assert!(
        !mask.contains(PauseReason::DangerousTemperature),
        "the acknowledgement must clear the halt"
    );

    // Tick 3: the same over-threshold sample must trip the halt again.
    // This is the half that proves the read is fresh. A local flag set
    // on tick 1 and never told about the acknowledgement would still
    // read as held, and `evaluate` would answer `Ok` at any temperature
    // for the rest of the loop's life.
    tick(driver, &mut thermal);
    assert_eq!(
        drain_messages(&MANAGER, &mut mask),
        std::vec![Message::DangerousTemperature],
        "the trip must re-arm once the manager's mask reports the halt cleared"
    );
}
