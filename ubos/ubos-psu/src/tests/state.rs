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
use crate::{BmmDevice, BmmPsuDescriptor};
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

/// Build a `Shared` over a mock bus, and return a second handle on the
/// same expectation queue. `Mock` clones share that queue through an
/// `Arc`, so the clone is how a test calls `done()` without reaching
/// past `Shared` for the bus it guards.
fn shared_with_bus(
    transactions: &[Transaction],
    config: BmmPsuDescriptor,
) -> (Shared<BmmDevice<Mock>>, Mock) {
    let bus = Mock::new(transactions);
    (Shared::new(bus.clone(), config), bus)
}

#[test]
fn acquire_runs_the_full_init_sequence() {
    let config = BmmPsuDescriptor::DEFAULT;
    let (shared, mut bus) = shared_with_bus(&test_support::init_transactions(&config), config);
    run(shared.acquire()).expect("BUG: init failed");
    bus.done();
}

#[test]
fn acquire_runs_init_once_and_caches_the_success() {
    let config = BmmPsuDescriptor::DEFAULT;
    let (shared, mut bus) = shared_with_bus(&test_support::init_transactions(&config), config);
    // The guard has to go before the second call: `acquire` takes the
    // same lock and would otherwise wait on a future nothing wakes.
    drop(run(shared.acquire()).expect("BUG: first init failed"));
    // The mock holds exactly one init sequence, so a second run of it
    // would fail here rather than at `done` below.
    drop(run(shared.acquire()).expect("BUG: cached init returned error"));
    bus.done();
}

#[test]
fn acquire_caches_the_failure_kind() {
    let config = BmmPsuDescriptor::DEFAULT;
    // First transaction errors out: NACK on the heartbeat write.
    let expectations = [
        Transaction::write(config.heartbeat_address, vec![0x7a, 0x00]).with_error(
            embedded_hal_async::i2c::ErrorKind::NoAcknowledge(
                embedded_hal_async::i2c::NoAcknowledgeSource::Address,
            ),
        ),
    ];
    let (shared, mut bus) = shared_with_bus(&expectations, config);
    let first = run(shared.acquire());
    assert!(matches!(first.err(), Some(Error::Bus(_))));
    // The second caller never reaches the bus and reports the kind the
    // first attempt recorded.
    let second = run(shared.acquire());
    assert!(matches!(
        second.err(),
        Some(Error::InitFailed(ErrorKind::NoAcknowledge(_)))
    ));
    bus.done();
}

/// An I2C bus that suspends once inside every transaction.
///
/// `Mock` finishes a transaction in a single poll, so a span holding the
/// lock over it offers no point at which a second caller can run. This
/// wrapper yields first, which is what makes the window below reachable
/// from a test.
struct SlowMock(Mock);

impl embedded_hal_async::i2c::ErrorType for SlowMock {
    type Error = <Mock as embedded_hal_async::i2c::ErrorType>::Error;
}

// Every method is forwarded rather than left to the trait's default
// bodies. `Mock` matches an expectation against the method that
// consumed it, so a `write` routed through `transaction` fails on the
// mode instead of the bytes.
impl embedded_hal_async::i2c::I2c for SlowMock {
    async fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        embassy_futures::yield_now().await;
        self.0.read(address, buffer).await
    }

    async fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
        embassy_futures::yield_now().await;
        self.0.write(address, bytes).await
    }

    async fn write_read(
        &mut self,
        address: u8,
        bytes: &[u8],
        buffer: &mut [u8],
    ) -> Result<(), Self::Error> {
        embassy_futures::yield_now().await;
        self.0.write_read(address, bytes, buffer).await
    }

    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [embedded_hal_async::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        embassy_futures::yield_now().await;
        self.0.transaction(address, operations).await
    }
}

#[test]
fn a_terminate_taken_while_a_caller_queues_refuses_that_caller() {
    let config = BmmPsuDescriptor::DEFAULT;
    let bus = Mock::new(&test_support::init_transactions(&config));
    let mut expectations = bus.clone();
    let shared = Shared::<BmmDevice<SlowMock>>::new(SlowMock(bus), config);

    // `join3` polls the three in order on every wake, which fixes the
    // interleaving the first poll produces:
    //
    // 1. the holder takes the lock and suspends inside init,
    // 2. the waiter reads the flag as clear and queues on the lock,
    // 3. the stopper sets the flag and queues behind the waiter.
    //
    // The waiter is therefore holding an answer that was true when it
    // read it and is false by the time the lock reaches it. Only the
    // second read, under the guard, can catch that.
    let mut refused = None;
    let mut stopped = None;
    run(embassy_futures::join::join3(
        async {
            drop(shared.acquire().await.expect("BUG: holder init failed"));
        },
        async {
            refused = Some(shared.acquire().await.err());
        },
        async {
            stopped = Some(shared.terminate().await.is_some());
        },
    ));

    assert!(
        matches!(refused, Some(Some(Error::Terminated))),
        "BUG: a caller that queued behind a terminate was served"
    );
    // The stopper found the device up, which is the case that has a
    // final device-off command to send.
    assert_eq!(stopped, Some(true));
    // The waiter reached no register of its own.
    expectations.done();
}

#[test]
fn a_terminate_taken_while_a_heartbeat_queues_refuses_that_heartbeat() {
    let config = BmmPsuDescriptor::DEFAULT;
    let bus = Mock::new(&test_support::init_transactions(&config));
    let mut expectations = bus.clone();
    let shared = Shared::<BmmDevice<SlowMock>>::new(SlowMock(bus), config);

    // The same interleaving as above, with the heartbeat's entry point as
    // the waiter. `initialized` reads no flag before the lock, so the
    // window is wider here than for `acquire`: the heartbeat is already
    // queued when the stopper sets the flag.
    //
    // 1. the holder takes the lock and suspends inside init,
    // 2. the heartbeat queues on the lock,
    // 3. the stopper sets the flag and queues behind the heartbeat.
    //
    // The holder then records a successful init and releases the lock, so
    // the heartbeat wakes to an init status that says the device is up and
    // a flag that says it was stopped. The init status is a cache that
    // `terminate` does not clear, so only the flag read under the guard
    // separates the two.
    let mut heartbeat = None;
    let mut stopped = None;
    run(embassy_futures::join::join3(
        async {
            drop(shared.acquire().await.expect("BUG: holder init failed"));
        },
        async {
            heartbeat = Some(shared.initialized().await.is_some());
        },
        async {
            stopped = Some(shared.terminate().await.is_some());
        },
    ));

    assert_eq!(
        heartbeat,
        Some(false),
        "BUG: a heartbeat that queued behind a terminate was served"
    );
    // The stopper found the device up, so the init cache the heartbeat
    // read was telling the truth about init and nothing else.
    assert_eq!(stopped, Some(true));
    // The heartbeat reached no register of its own.
    expectations.done();
}
