//! Tests for the per-slot channel pairs and the active-slot switch.
//!
//! The dispatcher-side behavior (reading the active slot's cell on a
//! work tick) lives in `ubos-mining` and is covered there; what is
//! testable here is the routing contract the dispatcher relies on:
//! slot isolation in both directions, teardown clearing, and the
//! active-slot atomic.

use super::*;

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

fn solution(work_id: u32) -> Solution {
    Solution {
        work_id,
        nonce: 0xdead_beef,
        ntime: 0x6000_0001,
        version_bits: 0,
    }
}

#[test]
fn work_publishes_stay_in_their_slot() {
    let session: AsicSession<3> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(100));
    session.pool_handle(2).publish_work(assignment(200));

    let mut receiver_0 = session.work_receiver(0).expect("receiver slot free");
    let mut receiver_1 = session.work_receiver(1).expect("receiver slot free");
    let mut receiver_2 = session.work_receiver(2).expect("receiver slot free");

    assert_eq!(receiver_0.try_get().expect("slot 0 published").work_id, 100);
    assert!(receiver_1.try_get().is_none());
    assert_eq!(receiver_2.try_get().expect("slot 2 published").work_id, 200);
}

#[test]
fn work_cell_is_latest_wins_per_slot() {
    let session: AsicSession<2> = AsicSession::new();
    let handle = session.pool_handle(1);
    handle.publish_work(assignment(1));
    handle.publish_work(assignment(2));

    let mut receiver = session.work_receiver(1).expect("receiver slot free");
    assert_eq!(receiver.try_get().expect("published").work_id, 2);
}

#[test]
fn clear_work_empties_only_the_cleared_slot() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(1));
    session.pool_handle(1).publish_work(assignment(2));

    session.pool_handle(0).clear_work();

    let mut receiver_0 = session.work_receiver(0).expect("receiver slot free");
    let mut receiver_1 = session.work_receiver(1).expect("receiver slot free");
    assert!(receiver_0.try_get().is_none());
    assert_eq!(receiver_1.try_get().expect("untouched").work_id, 2);
}

#[test]
fn solutions_route_to_the_submitted_slot() {
    let session: AsicSession<3> = AsicSession::new();
    assert!(session.submit_solution(1, solution(7)));

    assert!(session.pool_handle(0).try_take_solution().is_none());
    assert!(session.pool_handle(2).try_take_solution().is_none());
    let routed = session
        .pool_handle(1)
        .try_take_solution()
        .expect("routed to slot 1");
    assert_eq!(routed.work_id, 7);
    // Drained: the queue holds no second copy.
    assert!(session.pool_handle(1).try_take_solution().is_none());
}

#[test]
fn submit_solution_reports_full_queue() {
    let session: AsicSession<1> = AsicSession::new();
    for i in 0..SOLUTION_QUEUE_DEPTH {
        assert!(session.submit_solution(0, solution(u32::try_from(i).expect("small"))));
    }
    assert!(!session.submit_solution(0, solution(99)));
}

#[test]
fn active_slot_defaults_to_zero_and_follows_set_active() {
    let session: AsicSession<3> = AsicSession::new();
    assert_eq!(session.active_slot(), 0);
    session.set_active(2);
    assert_eq!(session.active_slot(), 2);
    session.set_active(0);
    assert_eq!(session.active_slot(), 0);
}

#[test]
#[should_panic(expected = "active slot out of range")]
fn set_active_rejects_out_of_range_slot() {
    let session: AsicSession<2> = AsicSession::new();
    session.set_active(2);
}

#[test]
#[should_panic(expected = "pool slot out of range")]
fn pool_handle_rejects_out_of_range_slot() {
    let session: AsicSession<2> = AsicSession::new();
    let _ = session.pool_handle(2);
}
