//! Tests for the slot-aware work dispatch: which pool slot's cell a
//! work tick reads, what the registry records, and the empty-slot
//! skip.
//!
//! The submit half of the routing (`dispatch_response` pushing into
//! `solutions[slot.session_slot]`) is a registry read plus a
//! one-line `submit_solution` call; exercising it end-to-end would
//! mean crafting a chip response whose hash meets the diff-1 target.
//! What matters for routing — that each registry entry pins the pool
//! slot that was active at its dispatch, unaffected by later
//! switches — is covered directly here.

use super::*;

use embassy_futures::block_on;
use stratum_client::AsicSession;

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

/// Per-test dispatch harness: one `AsicSession<N>`, its receivers,
/// and the counters `dispatch_work` mutates.
struct Harness<'a, const N: usize> {
    session: &'a AsicSession<N>,
    receivers: [DynReceiver<'a, WorkAssignment>; N],
    serial: MockSerial,
    chip_work_id: u8,
    registry: [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
}

impl<'a, const N: usize> Harness<'a, N> {
    fn new(session: &'a AsicSession<N>) -> Self {
        Self {
            session,
            receivers: core::array::from_fn(|slot| {
                session
                    .work_receiver(slot)
                    .expect("test session has a free receiver per slot")
            }),
            serial: MockSerial::new(),
            chip_work_id: 0,
            registry: [const { None }; WORK_REGISTRY_SLOTS],
        }
    }

    fn dispatch(&mut self) {
        const MIDSTATE_COUNT: u8 = 8;
        block_on(Serial::dispatch_work(
            &mut self.serial,
            self.session,
            &mut self.chip_work_id,
            MIDSTATE_COUNT,
            u8::MAX,
            &mut self.registry,
            &mut self.receivers,
        ));
    }
}

#[test]
fn dispatch_reads_the_active_slot() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(1).publish_work(assignment(20));
    session.set_active(1);

    let mut harness = Harness::new(&session);
    harness.dispatch();

    let slot = harness.registry[0].as_ref().expect("work dispatched");
    assert_eq!(slot.assignment.work_id, 20);
    assert_eq!(slot.session_slot, 1);
    assert!(!harness.serial.written.is_empty(), "frame reached the chip");
}

#[test]
fn dispatch_skips_an_empty_active_slot() {
    let session: AsicSession<2> = AsicSession::new();
    // Slot 0 has work, but the active slot 1 has published nothing.
    session.pool_handle(0).publish_work(assignment(10));
    session.set_active(1);

    let mut harness = Harness::new(&session);
    harness.dispatch();

    assert!(harness.registry.iter().all(Option::is_none));
    assert!(harness.serial.written.is_empty());
    assert_eq!(harness.chip_work_id, 0, "skipped tick burns no chip id");
}

#[test]
fn dispatch_skips_a_cleared_active_slot() {
    let session: AsicSession<1> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(0).clear_work();

    let mut harness = Harness::new(&session);
    harness.dispatch();

    assert!(harness.registry.iter().all(Option::is_none));
    assert!(harness.serial.written.is_empty());
}

#[test]
fn registry_pins_the_dispatch_time_slot_across_a_switch() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(1).publish_work(assignment(20));

    let mut harness = Harness::new(&session);
    // First dispatch while slot 0 is active, then switch mid-flight
    // and dispatch again.
    harness.dispatch();
    session.set_active(1);
    harness.dispatch();

    let first = harness.registry[0].as_ref().expect("first dispatch");
    let second = harness.registry[1].as_ref().expect("second dispatch");
    // The first entry still routes to slot 0: a solution arriving for
    // it after the switch goes back to the session that owns the
    // work, not to the newly active one.
    assert_eq!(first.session_slot, 0);
    assert_eq!(first.assignment.work_id, 10);
    assert_eq!(second.session_slot, 1);
    assert_eq!(second.assignment.work_id, 20);
}
