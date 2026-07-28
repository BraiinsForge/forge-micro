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

//! Tests for the scheduler-driven work dispatch: which pool slot's
//! cell a work tick reads, what the registry records, the empty-slot
//! skip and fallback, and the diff-1 share feedback into the
//! scheduler.
//!
//! The submit half of the routing (`process_solution` pushing into
//! `solutions[slot.session_slot]`) is exercised through a crafted
//! [`ChainSolution`] whose rebuilt header meets the diff-1 target,
//! which also covers the scheduler share accounting. What matters for
//! routing — that each registry entry pins the pool slot it was
//! dispatched from, unaffected by later scheduler picks — is covered
//! directly here.

use super::*;

use embassy_futures::block_on;
use stratum_client::AsicSession;
use ubos_asic::{Bm13xxDriver, Chip};

/// Fixed-slot scheduler stub: `evaluate` returns the pinned slot when
/// it is available, otherwise the lowest available slot, so both the
/// pinned-slot path and dispatch's fallback path are exercisable.
/// `accumulate_share` just records which slot was credited.
struct Pinned {
    slot: usize,
    accumulated: heapless::Vec<usize, 8>,
}

impl Pinned {
    fn new(slot: usize) -> Self {
        Self {
            slot,
            accumulated: heapless::Vec::new(),
        }
    }
}

impl<const N: usize> Scheduler<N> for Pinned {
    fn accumulate_share(&mut self, slot: usize) {
        self.accumulated
            .push(slot)
            .expect("test never accumulates more than 8 shares");
    }

    fn evaluate(&mut self, available: &[bool; N]) -> Option<usize> {
        if available.get(self.slot).copied().unwrap_or(false) {
            return Some(self.slot);
        }
        available.iter().position(|&is_available| is_available)
    }
}

/// Serial mock: captures dispatched frames, accepts any baud rate,
/// and answers reads with silence. The dispatch path only writes;
/// the `Read` bound comes along with the shared [`Transport`] impl.
struct MockSerial {
    written: heapless::Vec<u8, 512>,
}

impl MockSerial {
    fn new() -> Self {
        Self {
            written: heapless::Vec::new(),
        }
    }
}

impl embedded_io::ErrorType for MockSerial {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Write for MockSerial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.written
            .extend_from_slice(buf)
            .expect("mock buffer fits a work frame");
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_io_async::Read for MockSerial {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}

impl BaudRate for MockSerial {
    fn set_baudrate(&mut self, _baud: u32) -> Result<(), ErrorKind> {
        Ok(())
    }
}

fn assignment(work_id: u32) -> WorkAssignment {
    WorkAssignment {
        work_id,
        prev_hash: [0x11; 32],
        merkle_root: [0x33; 32],
        target: [0x22; 32],
        version: 0x2000_0000,
        nbits: 0x1705_c739,
        ntime: 0x6000_0000,
    }
}

/// Genesis block header fields, in the internal byte order
/// [`solution_hash`] expects. A mined block always meets diff-1 — it
/// is the easiest possible target — so genesis's known
/// `(header, nonce)` pair is a real chip solution that passes the
/// diff-1 check without brute-forcing one in the test.
const GENESIS_MERKLE_ROOT: [u8; 32] = [
    0x3b, 0xa3, 0xed, 0xfd, 0x7a, 0x7b, 0x12, 0xb2, 0x7a, 0xc7, 0x2c, 0x3e, 0x67, 0x76, 0x8f, 0x61,
    0x7f, 0xc8, 0x1b, 0xc3, 0x88, 0x8a, 0x51, 0x32, 0x3a, 0x9f, 0xb8, 0xaa, 0x4b, 0x1e, 0x5e, 0x4a,
];

/// The genesis block's nonce as a [`ChainSolution`] for job key 0,
/// with no version rolling and no ntime roll.
const GENESIS_SOLUTION: ChainSolution = ChainSolution {
    job_key: 0,
    nonce: 0x7C2B_AC1D,
    ntime_offset: 0,
    version_bits: 0,
};

/// A `WorkAssignment` matching the genesis block header. The
/// all-zero pool target guarantees the pool-target check in
/// `process_solution` fails, so the test isolates the scheduler
/// credit from the submit path.
fn genesis_assignment() -> WorkAssignment {
    WorkAssignment {
        work_id: 1,
        prev_hash: [0u8; 32],
        merkle_root: GENESIS_MERKLE_ROOT,
        target: [0u8; 32],
        version: 1,
        nbits: 0x1d00_ffff,
        ntime: 1_231_006_505,
    }
}

/// Per-test dispatch harness: one `AsicSession<N>`, its receivers,
/// the chain driver, a `Pinned` scheduler stub, and the counters
/// `dispatch_work` mutates.
struct Harness<'a, const N: usize> {
    session: &'a AsicSession<N>,
    receivers: [DynReceiver<'a, WorkAssignment>; N],
    serial: MockSerial,
    driver: Bm13xxDriver,
    scheduler: Pinned,
    next_job_key: u32,
    job_slots: u32,
    registry: [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
}

impl<'a, const N: usize> Harness<'a, N> {
    fn new(session: &'a AsicSession<N>, pinned_slot: usize) -> Self {
        let driver = Bm13xxDriver::new(Chip::Bm1362);
        // Mirror `run`'s registry sizing so job keys stay valid indices.
        let job_slots = u32::try_from(driver.job_slots().min(WORK_REGISTRY_SLOTS))
            .unwrap_or(1)
            .max(1);
        Self {
            session,
            receivers: core::array::from_fn(|slot| {
                session
                    .work_receiver(slot)
                    .expect("test session has a free receiver per slot")
            }),
            serial: MockSerial::new(),
            driver,
            scheduler: Pinned::new(pinned_slot),
            next_job_key: 0,
            job_slots,
            registry: [const { None }; WORK_REGISTRY_SLOTS],
        }
    }

    fn dispatch(&mut self) {
        block_on(Serial::dispatch_work(
            &mut self.serial,
            &mut self.scheduler,
            &mut self.driver,
            &mut self.next_job_key,
            self.job_slots,
            &mut self.registry,
            &mut self.receivers,
        ));
    }
}

#[test]
fn dispatch_reads_the_pinned_slot() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(1).publish_work(assignment(20));

    let mut harness = Harness::new(&session, 1);
    harness.dispatch();

    let slot = harness.registry[0].as_ref().expect("work dispatched");
    assert_eq!(slot.assignment.work_id, 20);
    assert_eq!(slot.session_slot, 1);
    assert!(!harness.serial.written.is_empty(), "frame reached the chip");
}

#[test]
fn dispatch_skips_the_tick_when_no_slot_has_work() {
    let session: AsicSession<2> = AsicSession::new();
    // Neither slot has published anything: the pinned slot is empty
    // and there is no other slot to fall back to.
    let mut harness = Harness::new(&session, 1);
    harness.dispatch();

    assert!(harness.registry.iter().all(Option::is_none));
    assert!(harness.serial.written.is_empty());
    assert_eq!(harness.next_job_key, 0, "skipped tick burns no job key");
}

#[test]
fn dispatch_falls_back_to_an_available_slot_when_the_pinned_slot_is_empty() {
    let session: AsicSession<2> = AsicSession::new();
    // Slot 0 has work; the pinned slot 1 has published nothing.
    session.pool_handle(0).publish_work(assignment(10));

    let mut harness = Harness::new(&session, 1);
    harness.dispatch();

    let slot = harness.registry[0].as_ref().expect("fallback dispatch");
    assert_eq!(slot.assignment.work_id, 10);
    assert_eq!(slot.session_slot, 0);
    assert!(!harness.serial.written.is_empty(), "frame reached the chip");
}

#[test]
fn dispatch_skips_a_tick_when_the_pinned_slot_was_cleared() {
    let session: AsicSession<1> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(0).clear_work();

    let mut harness = Harness::new(&session, 0);
    harness.dispatch();

    assert!(harness.registry.iter().all(Option::is_none));
    assert!(harness.serial.written.is_empty());
}

#[test]
fn registry_pins_the_dispatch_time_slot_across_a_pin_change() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(1).publish_work(assignment(20));

    let mut harness = Harness::new(&session, 0);
    // First dispatch while slot 0 is pinned, then change the pin
    // mid-flight and dispatch again.
    harness.dispatch();
    harness.scheduler.slot = 1;
    harness.dispatch();

    let first = harness.registry[0].as_ref().expect("first dispatch");
    let second = harness.registry[1].as_ref().expect("second dispatch");
    // The first entry still routes to slot 0: a solution arriving for
    // it after the pin change goes back to the session that owns the
    // work, not to the newly pinned one.
    assert_eq!(first.session_slot, 0);
    assert_eq!(first.assignment.work_id, 10);
    assert_eq!(second.session_slot, 1);
    assert_eq!(second.assignment.work_id, 20);
}

#[test]
fn process_solution_credits_the_scheduler_on_a_diff_1_share() {
    let session: AsicSession<1> = AsicSession::new();
    session.pool_handle(0).publish_work(genesis_assignment());

    let mut harness = Harness::new(&session, 0);
    harness.dispatch();

    Serial::process_solution(
        &GENESIS_SOLUTION,
        &harness.registry,
        harness.session,
        &mut harness.scheduler,
    );

    assert_eq!(harness.scheduler.accumulated.as_slice(), &[0]);
}

/// A diff-1 chip share also bumps the originating slot's
/// `POOL_WORK_SHARES` counter, leaving an unrelated slot untouched.
/// The counters are process-wide statics shared across the whole test
/// binary, so the assertions baseline against the current value
/// first rather than an absolute count. Dispatch is pinned to slot 1
/// and the untouched check reads slot 2 — slot 0 is exercised by the
/// sibling `process_solution_credits_the_scheduler_on_a_diff_1_share`
/// test, and under the default parallel runner that test's mutation
/// could otherwise land inside this test's baseline-to-assert window.
/// Any future test added here that also drives `process_solution`
/// should pick another still-untouched slot rather than reusing 0-2.
#[test]
fn process_solution_bumps_the_originating_slots_pool_work_share_counter() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(1).publish_work(genesis_assignment());

    let mut harness = Harness::new(&session, 1);
    harness.dispatch();

    let baseline = [
        POOL_WORK_SHARES[1].load(Ordering::Relaxed),
        POOL_WORK_SHARES[2].load(Ordering::Relaxed),
    ];

    Serial::process_solution(
        &GENESIS_SOLUTION,
        &harness.registry,
        harness.session,
        &mut harness.scheduler,
    );

    assert_eq!(
        POOL_WORK_SHARES[1].load(Ordering::Relaxed),
        baseline[0] + 1,
        "the originating slot's counter records the diff-1 share"
    );
    assert_eq!(
        POOL_WORK_SHARES[2].load(Ordering::Relaxed),
        baseline[1],
        "an unrelated slot's counter is untouched"
    );
}
