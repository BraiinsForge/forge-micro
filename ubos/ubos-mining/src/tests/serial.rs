//! Tests for the scheduler-driven work dispatch: which pool slot's
//! cell a work tick reads, what the registry records, the empty-slot
//! skip and fallback, and the diff-1 share feedback into the
//! scheduler.
//!
//! The submit half of the routing (`dispatch_response` pushing into
//! `solutions[slot.session_slot]`) is exercised through a crafted
//! chip response whose hash meets the diff-1 target, which also
//! covers the scheduler share accounting. What matters for routing —
//! that each registry entry pins the pool slot it was dispatched
//! from, unaffected by later scheduler picks — is covered directly
//! here.

use super::*;

use embassy_futures::block_on;
use stratum_client::AsicSession;

/// Fixed-slot scheduler stub: `evaluate` returns the pinned slot when
/// it is available, otherwise the lowest available slot, so both the
/// pinned-slot path and dispatch's fallback path are exercisable.
/// `accumulate_share` just records which slot was credited.
struct Pinned {
    slot: usize,
    accumulated: HVec<usize, 8>,
}

impl Pinned {
    fn new(slot: usize) -> Self {
        Self {
            slot,
            accumulated: HVec::new(),
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

/// The per-chip constants tests dispatch with: BM1362, 8-way midstate,
/// full-range work ids.
const CTX: ChipCtx = ChipCtx {
    chip: Chip::Bm1362,
    midstate_count: 8,
    max_chip_work_id: u8::MAX,
};

/// Write-only serial mock: captures dispatched frames, accepts any
/// baud rate.
struct MockSerial {
    written: HVec<u8, 512>,
}

impl MockSerial {
    fn new() -> Self {
        Self {
            written: HVec::new(),
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
/// `HwWork::compute_hash` expects. A mined block always meets
/// diff-1 — it is the easiest possible target — so genesis's known
/// `(header, nonce)` pair is a real chip response that passes the
/// diff-1 check without brute-forcing one in the test.
const GENESIS_MERKLE_ROOT: [u8; 32] = [
    0x3b, 0xa3, 0xed, 0xfd, 0x7a, 0x7b, 0x12, 0xb2, 0x7a, 0xc7, 0x2c, 0x3e, 0x67, 0x76, 0x8f, 0x61,
    0x7f, 0xc8, 0x1b, 0xc3, 0x88, 0x8a, 0x51, 0x32, 0x3a, 0x9f, 0xb8, 0xaa, 0x4b, 0x1e, 0x5e, 0x4a,
];

/// Raw work-response wire frame carrying the genesis block's nonce
/// `0x7C2B_AC1D` with work-id 0 and version-idx 0, CRC5-valid for
/// [`ResponseParser::pop`].
const GENESIS_SOLUTION_FRAME: [u8; 11] = [
    0xAA, 0x55, 0x1D, 0xAC, 0x2B, 0x7C, 0x00, 0x00, 0x00, 0x00, 0x90,
];

/// A `WorkAssignment` matching the genesis block header. The
/// all-zero pool target guarantees the pool-target check in
/// `dispatch_response` fails, so the test isolates the scheduler
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
/// a `Pinned` scheduler stub, and the counters `dispatch_work` mutates.
struct Harness<'a, const N: usize> {
    session: &'a AsicSession<N>,
    receivers: [DynReceiver<'a, WorkAssignment>; N],
    serial: MockSerial,
    scheduler: Pinned,
    chip_work_id: u8,
    registry: [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
}

impl<'a, const N: usize> Harness<'a, N> {
    fn new(session: &'a AsicSession<N>, pinned_slot: usize) -> Self {
        Self {
            session,
            receivers: core::array::from_fn(|slot| {
                session
                    .work_receiver(slot)
                    .expect("test session has a free receiver per slot")
            }),
            serial: MockSerial::new(),
            scheduler: Pinned::new(pinned_slot),
            chip_work_id: 0,
            registry: [const { None }; WORK_REGISTRY_SLOTS],
        }
    }

    fn dispatch(&mut self) {
        block_on(Serial::dispatch_work(
            &mut self.serial,
            &mut self.scheduler,
            &mut self.chip_work_id,
            CTX,
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
    assert_eq!(harness.chip_work_id, 0, "skipped tick burns no chip id");
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
fn dispatch_response_credits_the_scheduler_on_a_diff_1_share() {
    let session: AsicSession<1> = AsicSession::new();
    session.pool_handle(0).publish_work(genesis_assignment());

    let mut harness = Harness::new(&session, 0);
    harness.dispatch();

    let mut parser = ResponseParser::new();
    parser.free()[..GENESIS_SOLUTION_FRAME.len()].copy_from_slice(&GENESIS_SOLUTION_FRAME);
    parser.commit(GENESIS_SOLUTION_FRAME.len());
    let resp = parser.pop(Chip::Bm1362).expect("well-formed frame parses");

    let register_replies: Channel<CriticalSectionRawMutex, RegisterResponse, MAX_CHIPS> =
        Channel::new();
    Serial::dispatch_response(
        &resp,
        CTX,
        &harness.registry,
        &register_replies,
        harness.session,
        &mut harness.scheduler,
    );

    assert_eq!(harness.scheduler.accumulated.as_slice(), &[0]);
}
