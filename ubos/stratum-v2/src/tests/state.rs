//! `V2SessionState` ring-buffer unit tests.

use mining_protocol::JobId;

use crate::state::{PendingFutureJob, V2SessionState, JOBID_MAP_DEPTH};

#[test]
fn default_matches_new() {
    assert_eq!(V2SessionState::default(), V2SessionState::new());
}

#[test]
fn new_uses_accept_all_target() {
    // 0xFF..FF is the accept-everything sentinel until SetTarget arrives.
    assert_eq!(V2SessionState::new().target, [0xFF; 32]);
}

#[test]
fn allocate_job_id_returns_monotonic_generic_ids() {
    let mut state = V2SessionState::new();

    let first = state.allocate_job_id(100);
    let second = state.allocate_job_id(101);

    assert_eq!(first, JobId(0));
    assert_eq!(second, JobId(1));
    assert_eq!(state.next_job_id, 2);
}

#[test]
fn allocated_job_id_maps_to_wire_id() {
    let mut state = V2SessionState::new();

    let generic = state.allocate_job_id(42);

    assert_eq!(state.lookup_wire_job_id(generic), Some(42));
}

#[test]
fn job_id_ring_rotates_after_depth() {
    let mut state = V2SessionState::new();

    let first = state.allocate_job_id(10);
    let second = state.allocate_job_id(11);

    assert_eq!(state.lookup_wire_job_id(first), Some(10));
    assert_eq!(state.lookup_wire_job_id(second), Some(11));
    assert_eq!(state.job_id_write_index, 0);

    let third = state.allocate_job_id(12);

    assert_eq!(JOBID_MAP_DEPTH, 2);
    assert_eq!(state.lookup_wire_job_id(first), None);
    assert_eq!(state.lookup_wire_job_id(second), Some(11));
    assert_eq!(state.lookup_wire_job_id(third), Some(12));
    assert_eq!(state.job_id_write_index, 1);
}

#[test]
fn lookup_unknown_job_id_returns_none() {
    let mut state = V2SessionState::new();

    state.allocate_job_id(10);

    assert_eq!(state.lookup_wire_job_id(JobId(999)), None);
}

#[test]
fn job_id_counter_wraps() {
    let mut state = V2SessionState::new();
    state.next_job_id = u32::MAX;

    let last = state.allocate_job_id(10);
    let wrapped = state.allocate_job_id(11);

    assert_eq!(last, JobId(u32::MAX));
    assert_eq!(wrapped, JobId(0));
    assert_eq!(state.next_job_id, 1);
}

#[test]
fn sequence_numbers_are_monotonic() {
    let mut state = V2SessionState::new();

    assert_eq!(state.allocate_sequence_number(), 0);
    assert_eq!(state.allocate_sequence_number(), 1);
    assert_eq!(state.allocate_sequence_number(), 2);
    assert_eq!(state.next_sequence_number, 3);
}

#[test]
fn sequence_number_counter_wraps() {
    let mut state = V2SessionState::new();
    state.next_sequence_number = u32::MAX;

    assert_eq!(state.allocate_sequence_number(), u32::MAX);
    assert_eq!(state.allocate_sequence_number(), 0);
    assert_eq!(state.next_sequence_number, 1);
}

#[test]
fn pending_future_job_can_be_stored() {
    let mut state = V2SessionState::new();
    let expected = PendingFutureJob {
        wire_job_id: 77,
        version: 0x2000_0000,
        merkle_root: [0xA5; 32],
    };

    state.pending_future_job = Some(expected);

    assert_eq!(state.pending_future_job, Some(expected));
}
