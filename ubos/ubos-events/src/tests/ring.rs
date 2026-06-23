//! Tests for `Ring` push/snapshot behaviour and the global-ring pre-init
//! safety guarantees.

use crate::{record, snapshot, EventRecord, Ring, Severity};

fn make_clock() -> fn() -> u64 {
    fn clock() -> u64 {
        0
    }
    clock
}

fn make_ring(capacity: usize) -> Ring {
    let boxed: Box<[EventRecord]> = vec![EventRecord::EMPTY; capacity].into_boxed_slice();
    let leaked: &'static mut [EventRecord] = Box::leak(boxed);
    Ring::new(leaked, make_clock())
}

#[test]
fn ring_push_and_snapshot_basic() {
    let mut ring = make_ring(4);
    ring.push(1, Severity::Info, &[]);
    ring.push(2, Severity::Warn, &[]);

    let mut out = [EventRecord::EMPTY; 4];
    let count = ring.snapshot(&mut out);
    assert_eq!(count, 2);
    assert_eq!(out[0].code, 1);
    assert_eq!(out[0].seq, 1);
    assert_eq!(out[1].code, 2);
    assert_eq!(out[1].seq, 2);
}

#[test]
fn ring_wrap_keeps_last_cap_records() {
    const CAPACITY: usize = 4;
    let mut ring = make_ring(CAPACITY);

    // Push CAPACITY + 3 = 7 records; only the last 4 should be in the ring.
    for code in 0u32..7 {
        ring.push(code, Severity::Info, &[]);
    }

    let mut out = [EventRecord::EMPTY; CAPACITY];
    let count = ring.snapshot(&mut out);
    assert_eq!(count, CAPACITY);

    // Oldest surviving record has code 3 (seq 4), newest has code 6 (seq 7).
    for (index, expected_code) in (3u32..7).enumerate() {
        assert_eq!(
            out[index].code, expected_code,
            "slot {index}: expected code {expected_code}, got {}",
            out[index].code
        );
        assert_eq!(
            out[index].seq,
            expected_code + 1,
            "slot {index}: expected seq {}",
            expected_code + 1
        );
    }
}

#[test]
fn ring_snapshot_oldest_first() {
    const CAPACITY: usize = 3;
    let mut ring = make_ring(CAPACITY);
    ring.push(10, Severity::Info, &[]);
    ring.push(20, Severity::Warn, &[]);
    ring.push(30, Severity::Error, &[]);
    // Ring is now exactly full; next push will overwrite slot 0.
    ring.push(40, Severity::Info, &[]);

    let mut out = [EventRecord::EMPTY; CAPACITY];
    let count = ring.snapshot(&mut out);
    assert_eq!(count, CAPACITY);
    // Oldest first: 20, 30, 40.
    assert_eq!(out[0].code, 20);
    assert_eq!(out[1].code, 30);
    assert_eq!(out[2].code, 40);
}

#[test]
fn ring_zero_capacity_is_safe() {
    let boxed: Box<[EventRecord]> = vec![].into_boxed_slice();
    let leaked: &'static mut [EventRecord] = Box::leak(boxed);
    let mut ring = Ring::new(leaked, make_clock());
    // Push and snapshot must not panic.
    ring.push(1, Severity::Info, &[]);
    let mut out = [EventRecord::EMPTY; 4];
    assert_eq!(ring.snapshot(&mut out), 0);
}

#[test]
fn pre_init_record_does_not_panic() {
    // The global RING may be uninitialised (None) when this test runs;
    // record must be safe regardless. We do not assert ring contents
    // because other tests may have called init on the shared global.
    record(crate::const_fnv1a("test_event"), Severity::Info, &[]);
}

#[test]
fn pre_init_snapshot_returns_zero_or_valid() {
    // snapshot must not panic; if RING is uninitialised it returns 0.
    // If init was called by another test, it returns a non-panic value.
    let mut out = [EventRecord::EMPTY; 4];
    let _ = snapshot(&mut out);
}
