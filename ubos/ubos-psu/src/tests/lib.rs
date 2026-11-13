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
use core::cell::{Cell, RefCell};
use core::convert::Infallible;
use core::future::{Future, pending, poll_fn};
use core::pin::pin;
use core::task::Poll as TaskPoll;
use embassy_futures::join::join;
use embassy_futures::yield_now;

#[test]
fn status_flags_default_all_clear() {
    let flags = StatusFlags::default();
    assert!(!flags.current_limit);
    assert!(!flags.thermal_shutdown);
    assert!(!flags.overvoltage);
    assert!(!flags.undervoltage);
}

#[test]
fn error_init_failed_carries_kind() {
    let err: Error<core::convert::Infallible> =
        Error::InitFailed(embedded_hal_async::i2c::ErrorKind::Bus);
    match err {
        Error::InitFailed(kind) => {
            assert!(matches!(kind, embedded_hal_async::i2c::ErrorKind::Bus));
        }
        _ => panic!("BUG: variant mismatch"),
    }
}

/// The step the fake supply lands every voltage command on.
const LANDED: Voltage = Voltage::from_mv(1200);

/// A driver that logs its calls and hangs or fails on request.
#[derive(Default)]
struct FakePsu {
    /// Each rail command as it starts, the end of each enable, and each
    /// output power read.
    calls: RefCell<Vec<&'static str>>,
    enable_hangs: Cell<bool>,
    set_voltage_hangs: Cell<bool>,
    heartbeat_interval: Cell<Option<embassy_time::Duration>>,
    heartbeats: Cell<u32>,
    heartbeat_hangs: Cell<bool>,
    reads_hang: Cell<bool>,
    terminated: Cell<bool>,
    polls: Cell<u32>,
    model_asks: Cell<u32>,
}

impl FakePsu {
    fn read<T>(&self, value: T) -> Result<T, Error<Infallible>> {
        if self.terminated.get() {
            Err(Error::Terminated)
        } else {
            Ok(value)
        }
    }
}

impl Psu for FakePsu {
    type BusError = Infallible;
    async fn enable(&self) -> Result<(), Error<Infallible>> {
        self.calls.borrow_mut().push("enable");
        while self.enable_hangs.get() {
            yield_now().await;
        }
        self.calls.borrow_mut().push("enabled");
        self.read(())
    }
    async fn disable(&self) -> Result<(), Error<Infallible>> {
        self.calls.borrow_mut().push("disable");
        self.read(())
    }
    async fn set_voltage(&self, _voltage: Voltage) -> Result<Voltage, Error<Infallible>> {
        self.calls.borrow_mut().push("set_voltage");
        while self.set_voltage_hangs.get() {
            yield_now().await;
        }
        self.read(LANDED)
    }
    async fn read_input_power(&self) -> Result<Power, Error<Infallible>> {
        self.polls.set(self.polls.get() + 1);
        while self.reads_hang.get() {
            yield_now().await;
        }
        self.read(Power::from_mw(0))
    }
    async fn read_output_power(&self) -> Result<Power, Error<Infallible>> {
        self.calls.borrow_mut().push("read_output_power");
        self.read(Power::from_mw(0))
    }
    async fn read_output_voltage(&self) -> Result<Voltage, Error<Infallible>> {
        self.read(Voltage::from_mv(0))
    }
    async fn read_status(&self) -> Result<StatusFlags, Error<Infallible>> {
        self.read(StatusFlags::default())
    }
    async fn model(&self) -> Option<ModelString> {
        self.model_asks.set(self.model_asks.get() + 1);
        None
    }
    async fn heartbeat_interval(&self) -> Option<embassy_time::Duration> {
        self.heartbeat_interval.get()
    }
    async fn heartbeat(&self) -> Result<(), Error<Infallible>> {
        self.heartbeats.set(self.heartbeats.get() + 1);
        while self.heartbeat_hangs.get() {
            yield_now().await;
        }
        self.read(())
    }
    async fn terminate(&self) {}
}

/// The interval the fake supply's heartbeat is sent at, short so a test
/// sees several heartbeats.
const HEARTBEAT_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_millis(5);

/// A supply over a fresh fake driver, both leaked so they are `'static`
/// the way a board's are.
fn fake_supply() -> (&'static FakePsu, &'static PowerSupply<FakePsu>) {
    let driver: &'static FakePsu = Box::leak(Box::default());
    (driver, Box::leak(Box::new(PowerSupply::new(driver))))
}

/// Run `future` to completion with the PSU task polled beside it.
fn beside<F: Future>(psu: &'static PowerSupply<FakePsu>, future: F) -> F::Output {
    match embassy_futures::block_on(select(future, psu.run())) {
        Either::First(output) => output,
        Either::Second(never) => never,
    }
}

/// Poll `future` until `done` holds, then drop it, as a caller that
/// loses a race is dropped.
async fn poll_until(future: impl Future, done: impl Fn() -> bool) {
    let mut future = pin!(future);
    poll_fn(|context| {
        if done() {
            return TaskPoll::Ready(());
        }
        let _ = future.as_mut().poll(context);
        context.waker().wake_by_ref();
        TaskPoll::Pending
    })
    .await;
}

#[test]
fn set_voltage_records_the_request_and_the_voltage_the_driver_answered() {
    let (_, psu) = fake_supply();
    let handle = psu.handle();
    let requested = Voltage::from_mv(1203);
    assert_eq!(handle.commanded_voltage(), None);
    let set = beside(psu, handle.set_voltage(requested));
    assert!(set.is_ok());
    assert_eq!(handle.commanded_voltage(), Some(requested));
    let reading = beside(psu, async {
        loop {
            if let Some(reading) = handle.current() {
                return reading;
            }
            yield_now().await;
        }
    });
    assert_eq!(reading.output_voltage_set, LANDED);
}

#[test]
fn newer_command_waits_for_the_running_one() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.enable_hangs.set(true);
    let disabled = beside(psu, async {
        poll_until(handle.enable(), || !driver.calls.borrow().is_empty()).await;
        let (disabled, ()) = join(handle.disable(), async {
            driver.enable_hangs.set(false);
        })
        .await;
        disabled
    });
    assert!(disabled.is_ok());
    assert_eq!(*driver.calls.borrow(), ["enable", "enabled", "disable"]);
}

#[test]
fn newer_command_drops_a_running_one_that_is_stuck() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.enable_hangs.set(true);
    let disabled = beside(psu, async {
        poll_until(handle.enable(), || !driver.calls.borrow().is_empty()).await;
        handle.disable().await
    });
    assert!(disabled.is_ok());
    assert_eq!(*driver.calls.borrow(), ["enable", "disable"]);
}

#[test]
fn voltage_write_dropped_as_stuck_clears_the_commanded_voltage() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    let requested = Voltage::from_mv(1203);
    assert!(beside(psu, handle.set_voltage(requested)).is_ok());
    driver.set_voltage_hangs.set(true);
    let disabled = beside(psu, async {
        poll_until(handle.set_voltage(requested), || {
            driver.calls.borrow().len() > 1
        })
        .await;
        handle.disable().await
    });
    assert!(disabled.is_ok());
    assert_eq!(handle.commanded_voltage(), None);
}

#[test]
fn heartbeat_is_sent_on_its_own_interval_beside_the_commands() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.heartbeat_interval.set(Some(HEARTBEAT_INTERVAL));
    let disabled = beside(psu, async {
        poll_until(pending::<()>(), || driver.heartbeats.get() > 1).await;
        handle.disable().await
    });
    assert!(disabled.is_ok());
    assert_eq!(*driver.calls.borrow(), ["disable"]);
}

#[test]
fn terminated_driver_gets_no_more_heartbeats() {
    let (driver, psu) = fake_supply();
    driver.heartbeat_interval.set(Some(HEARTBEAT_INTERVAL));
    driver.terminated.set(true);
    beside(psu, async {
        poll_until(pending::<()>(), || driver.heartbeats.get() > 0).await;
        embassy_time::Timer::after(HEARTBEAT_INTERVAL * 4).await;
    });
    assert_eq!(driver.heartbeats.get(), 1);
}

#[test]
fn heartbeat_held_past_its_interval_is_followed_by_one_pulse() {
    let (driver, psu) = fake_supply();
    driver.heartbeat_interval.set(Some(HEARTBEAT_INTERVAL));
    driver.heartbeat_hangs.set(true);
    beside(psu, async {
        poll_until(pending::<()>(), || driver.heartbeats.get() > 0).await;
        embassy_time::Timer::after(HEARTBEAT_INTERVAL * 8).await;
        driver.heartbeat_hangs.set(false);
        embassy_time::Timer::after(HEARTBEAT_INTERVAL / 2).await;
    });
    assert_eq!(driver.heartbeats.get(), 2);
}

#[test]
fn command_is_answered_behind_a_heartbeat_that_is_stuck() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.heartbeat_interval.set(Some(HEARTBEAT_INTERVAL));
    driver.heartbeat_hangs.set(true);
    let disabled = beside(psu, async {
        poll_until(pending::<()>(), || driver.heartbeats.get() > 0).await;
        handle.disable().await
    });
    assert!(disabled.is_ok());
    assert_eq!(*driver.calls.borrow(), ["disable"]);
}

#[test]
fn command_drops_a_poll_read_that_is_stuck() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.reads_hang.set(true);
    let disabled = beside(psu, async {
        poll_until(pending::<()>(), || driver.polls.get() > 0).await;
        handle.disable().await
    });
    assert!(disabled.is_ok());
}

#[test]
fn queued_command_of_a_dropped_caller_is_not_started() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    // No task runs yet, so the enable is queued and its caller dropped.
    assert!(embassy_futures::poll_once(handle.enable()).is_pending());
    let disabled = beside(psu, handle.disable());
    assert!(disabled.is_ok());
    assert_eq!(*driver.calls.borrow(), ["disable"]);
}

#[test]
fn poll_stops_for_a_queued_command_and_runs_again() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.reads_hang.set(true);
    let disabled = beside(psu, async {
        poll_until(pending::<()>(), || driver.polls.get() > 0).await;
        let (disabled, ()) = join(handle.disable(), async {
            driver.reads_hang.set(false);
        })
        .await;
        poll_until(pending::<()>(), || driver.calls.borrow().len() > 1).await;
        disabled
    });
    assert!(disabled.is_ok());
    assert_eq!(*driver.calls.borrow(), ["disable", "read_output_power"]);
    assert_eq!(driver.polls.get(), 2);
}

#[test]
fn heartbeat_is_sent_between_the_commands_that_stop_a_poll() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.heartbeat_interval.set(Some(HEARTBEAT_INTERVAL));
    driver.reads_hang.set(true);
    let (before, after) = beside(psu, async {
        poll_until(pending::<()>(), || driver.polls.get() > 0).await;
        let before = driver.heartbeats.get();
        // Every command stops the poll, which then runs again and hangs.
        for _ in 0..3 {
            assert!(handle.disable().await.is_ok());
        }
        (before, driver.heartbeats.get())
    });
    assert!(after > before);
    assert!(driver.polls.get() > 1);
}

#[test]
fn terminated_driver_stops_the_polls_and_still_answers() {
    let (driver, psu) = fake_supply();
    let handle = psu.handle();
    driver.terminated.set(true);
    let enabled = beside(psu, async {
        poll_until(pending::<()>(), || driver.polls.get() > 0).await;
        embassy_time::Timer::after(POWER_STATS_INTERVAL + POWER_STATS_INTERVAL).await;
        handle.enable().await
    });
    assert!(matches!(enabled, Err(Error::Terminated)));
    assert_eq!(driver.polls.get(), 1);
}

#[test]
fn driver_that_reports_no_model_is_asked_once() {
    let (driver, psu) = fake_supply();
    beside(psu, poll_until(pending::<()>(), || driver.polls.get() > 2));
    assert_eq!(driver.model_asks.get(), 1);
}
