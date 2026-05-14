//! `PendingRequests` ring-buffer unit tests.

use crate::state::{PendingRequestKind, PendingRequests};

#[test]
fn register_then_take_returns_kind() {
    let mut pending: PendingRequests<4> = PendingRequests::new();
    let id = pending.register(PendingRequestKind::Subscribe);
    assert!(matches!(
        pending.take(id),
        Some(PendingRequestKind::Subscribe)
    ));
}

#[test]
fn take_clears_the_slot_so_second_take_is_none() {
    let mut pending: PendingRequests<4> = PendingRequests::new();
    let id = pending.register(PendingRequestKind::Configure);
    let _ = pending.take(id);
    assert!(pending.take(id).is_none());
}

#[test]
fn take_unknown_id_returns_none() {
    let mut pending: PendingRequests<4> = PendingRequests::new();
    pending.register(PendingRequestKind::Configure);
    assert!(pending.take(9999).is_none());
}

#[test]
fn submit_kind_carries_pool_difficulty_to_take() {
    let mut pending: PendingRequests<4> = PendingRequests::new();
    let id = pending.register(PendingRequestKind::Submit {
        pool_difficulty: 1024,
    });
    match pending.take(id) {
        Some(PendingRequestKind::Submit { pool_difficulty }) => {
            assert_eq!(pool_difficulty, 1024);
        }
        other => panic!("expected Submit with pool_difficulty=1024, got {other:?}"),
    }
}

#[test]
fn collision_evicts_older_request_silently() {
    // With N=4, ids 1 and 5 collide at slot 1.
    let mut pending: PendingRequests<4> = PendingRequests::new();
    let first = pending.register(PendingRequestKind::Subscribe); // id=1, slot 1
    let _ = pending.register(PendingRequestKind::Configure); // id=2, slot 2
    let _ = pending.register(PendingRequestKind::Authorize); // id=3, slot 3
    let _ = pending.register(PendingRequestKind::Configure); // id=4, slot 0
    let collider = pending.register(PendingRequestKind::Authorize); // id=5, slot 1 (overwrites first)
    assert_eq!(first % 4, collider % 4);
    // The evicted entry's response no longer resolves.
    assert!(pending.take(first).is_none());
    // The new occupant resolves fine.
    assert!(matches!(
        pending.take(collider),
        Some(PendingRequestKind::Authorize)
    ));
}

#[test]
fn ids_are_monotonic() {
    let mut pending: PendingRequests<4> = PendingRequests::new();
    let a = pending.register(PendingRequestKind::Configure);
    let b = pending.register(PendingRequestKind::Subscribe);
    let c = pending.register(PendingRequestKind::Authorize);
    assert!(b > a);
    assert!(c > b);
}
