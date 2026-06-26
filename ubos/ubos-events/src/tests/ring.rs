//! Tests for `Ring` push/snapshot behaviour and the global-ring pre-init
//! safety guarantees.

use crate::{record, snapshot_after, EventRecord, RetainedHeader, RetainedRing, Ring, Severity};

fn make_clock() -> fn() -> u64 {
    fn clock() -> u64 {
        0
    }
    clock
}

fn make_ring(capacity: usize) -> Ring {
    let header: &'static mut RetainedHeader = Box::leak(Box::new(RetainedHeader::FRESH));
    let entries: &'static mut [EventRecord] =
        Box::leak(vec![EventRecord::EMPTY; capacity].into_boxed_slice());
    Ring {
        header,
        entries,
        clock: make_clock(),
    }
}

#[test]
fn ring_push_and_snapshot_basic() {
    let mut ring = make_ring(4);
    ring.push(1, Severity::Info, &[]);
    ring.push(2, Severity::Warn, &[]);

    let mut out = [EventRecord::EMPTY; 4];
    let count = ring.snapshot_after(&mut out, 0);
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
    let count = ring.snapshot_after(&mut out, 0);
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
    let count = ring.snapshot_after(&mut out, 0);
    assert_eq!(count, CAPACITY);
    // Oldest first: 20, 30, 40.
    assert_eq!(out[0].code, 20);
    assert_eq!(out[1].code, 30);
    assert_eq!(out[2].code, 40);
}

#[test]
fn ring_snapshot_after_filters_by_seq() {
    let mut ring = make_ring(8);
    for code in 1u32..=5 {
        ring.push(code, Severity::Info, &[]);
    }
    // Records have seq 1..=5. Asking for seq > 2 yields the last three.
    let mut out = [EventRecord::EMPTY; 8];
    let count = ring.snapshot_after(&mut out, 2);
    assert_eq!(count, 3);
    assert_eq!(out[0].seq, 3);
    assert_eq!(out[1].seq, 4);
    assert_eq!(out[2].seq, 5);
    // A cursor at or past the latest seq yields nothing.
    assert_eq!(ring.snapshot_after(&mut out, 5), 0);
    assert_eq!(ring.latest_seq(), 5);
}

#[test]
fn ring_snapshot_after_streams_in_batches() {
    let mut ring = make_ring(16);
    for code in 1u32..=10 {
        ring.push(code, Severity::Info, &[]);
    }
    // Drain the whole ring two records at a time using a seq cursor, the
    // pattern the HTTP handler uses to avoid a large buffer.
    let latest = ring.latest_seq();
    let mut cursor = 0u32;
    let mut seen = Vec::new();
    while cursor < latest {
        let mut batch = [EventRecord::EMPTY; 2];
        let count = ring.snapshot_after(&mut batch, cursor);
        if count == 0 {
            break;
        }
        for record in &batch[..count] {
            seen.push(record.seq);
            cursor = record.seq;
        }
    }
    assert_eq!(seen, (1u32..=10).collect::<Vec<_>>());
}

#[test]
fn ring_snapshot_after_on_wrapped_ring() {
    // Overfill a capacity-4 ring with 7 pushes so it wraps: only seq 4..=7
    // remain live, with the oldest at head. snapshot_after must walk from the
    // wrapped oldest, not index 0.
    const CAPACITY: usize = 4;
    let mut ring = make_ring(CAPACITY);
    for code in 1u32..=7 {
        ring.push(code, Severity::Info, &[]);
    }
    // A cursor at seq 5 yields the two newest live records, 6 and 7.
    let mut out = [EventRecord::EMPTY; CAPACITY];
    let count = ring.snapshot_after(&mut out, 5);
    assert_eq!(count, 2);
    assert_eq!(out[0].seq, 6);
    assert_eq!(out[1].seq, 7);
    // A cursor older than the oldest live seq yields every live record.
    let count = ring.snapshot_after(&mut out, 2);
    assert_eq!(count, CAPACITY);
    assert_eq!(out[0].seq, 4);
    assert_eq!(out[3].seq, 7);
}

#[test]
fn ring_snapshot_after_streams_in_batches_on_wrapped_ring() {
    // Drain a wrapped ring with a seq cursor, the HTTP handler's pattern. Only
    // the last CAPACITY records survive the wrap, so the stream starts at 7.
    const CAPACITY: usize = 4;
    let mut ring = make_ring(CAPACITY);
    for code in 1u32..=10 {
        ring.push(code, Severity::Info, &[]);
    }
    let latest = ring.latest_seq();
    let mut cursor = 0u32;
    let mut seen = Vec::new();
    while cursor < latest {
        let mut batch = [EventRecord::EMPTY; 2];
        let count = ring.snapshot_after(&mut batch, cursor);
        if count == 0 {
            break;
        }
        for record in &batch[..count] {
            seen.push(record.seq);
            cursor = record.seq;
        }
    }
    assert_eq!(seen, (7u32..=10).collect::<Vec<_>>());
}

#[test]
fn header_is_valid_only_for_matching_build_and_ranges() {
    let mut header = RetainedHeader::FRESH;
    assert!(header.is_valid(8));
    header.set_head_len(3, 3);
    assert!(header.is_valid(8));

    let mut bad_magic = RetainedHeader::FRESH;
    bad_magic.magic = 0;
    assert!(!bad_magic.is_valid(8));

    let mut bad_layout = RetainedHeader::FRESH;
    bad_layout.layout = 999;
    assert!(!bad_layout.is_valid(8));

    let mut head_oob = RetainedHeader::FRESH;
    head_oob.set_head_len(8, 0);
    assert!(!head_oob.is_valid(8));

    let mut len_oob = RetainedHeader::FRESH;
    len_oob.set_head_len(0, 9);
    assert!(!len_oob.is_valid(8));
}

#[test]
fn retained_ring_adopts_a_valid_prior_session() {
    // A backing that survived a warm reset: valid header plus prior entries.
    let backing: &'static mut RetainedRing<8> = Box::leak(Box::new(RetainedRing::new()));
    backing.reset();
    for code in 1u32..=3 {
        backing.entries[(code - 1) as usize] = EventRecord {
            seq: code,
            ts: 0,
            code,
            severity: 0,
            args: [0u8; 11],
        };
    }
    backing.header.seq = 3;
    backing.header.set_head_len(3, 3);
    // init would see a valid header and adopt rather than reset.
    assert!(backing.header.is_valid(8));

    let RetainedRing { header, entries } = backing;
    let entries: &'static mut [EventRecord] = entries;
    let ring = Ring {
        header,
        entries,
        clock: make_clock(),
    };
    let mut out = [EventRecord::EMPTY; 8];
    assert_eq!(ring.snapshot_after(&mut out, 0), 3);
    assert_eq!(out[0].seq, 1);
    assert_eq!(out[2].seq, 3);
    assert_eq!(ring.latest_seq(), 3);
}

#[test]
fn retained_ring_resets_garbage() {
    // Cold-boot-style garbage: nonzero indices but no valid magic.
    let backing: &'static mut RetainedRing<8> = Box::leak(Box::new(RetainedRing::new()));
    backing.header.seq = 999;
    backing.header.set_head_len(5, 5);
    assert!(!backing.header.is_valid(8));

    backing.reset();
    assert!(backing.header.is_valid(8));
    assert_eq!(backing.header.len(), 0);
    assert_eq!(backing.header.seq, 0);
}

#[test]
fn ring_zero_capacity_is_safe() {
    let mut ring = make_ring(0);
    // Push and snapshot must not panic.
    ring.push(1, Severity::Info, &[]);
    let mut out = [EventRecord::EMPTY; 4];
    assert_eq!(ring.snapshot_after(&mut out, 0), 0);
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
    // snapshot_after must not panic; if RING is uninitialised it returns 0.
    // If init was called by another test, it returns a non-panic value.
    let mut out = [EventRecord::EMPTY; 4];
    let _ = snapshot_after(&mut out, 0);
}
