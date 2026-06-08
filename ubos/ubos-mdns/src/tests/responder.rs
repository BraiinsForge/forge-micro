use static_cell::StaticCell;

use super::{compose_hostname_configured, HOSTNAME_SUFFIX_LEN};

// Minimum CAP for base "ab" (BASE_LEN=2): 2 + 3 = 5.
const BASE: &str = "ab";
const BASE_LEN: usize = 2;
const CAP: usize = BASE_LEN + HOSTNAME_SUFFIX_LEN; // 5

#[test]
fn empty_configured_returns_composed() {
    static CELL: StaticCell<heapless::String<CAP>> = StaticCell::new();
    let hostname = compose_hostname_configured::<CAP, BASE_LEN>("", BASE, 0xef, &CELL);
    assert!(
        hostname.contains(BASE),
        "expected base in hostname, got {hostname}"
    );
    assert!(
        hostname.contains("ef"),
        "expected hex suffix in hostname, got {hostname}"
    );
}

#[test]
fn short_configured_returned_verbatim() {
    // CAP=5 fits "hello" (5 bytes).
    static CELL: StaticCell<heapless::String<CAP>> = StaticCell::new();
    let hostname = compose_hostname_configured::<CAP, BASE_LEN>("hello", BASE, 0x01, &CELL);
    assert_eq!(hostname, "hello");
}

#[test]
fn over_long_configured_falls_back_to_composed() {
    // "toolong" is 7 bytes, which does not fit in CAP=5.
    static CELL: StaticCell<heapless::String<CAP>> = StaticCell::new();
    let hostname = compose_hostname_configured::<CAP, BASE_LEN>("toolong", BASE, 0xab, &CELL);
    assert!(
        hostname.contains(BASE),
        "expected fallback to composed hostname, got {hostname}",
    );
    assert!(
        hostname.contains("ab"),
        "expected hex suffix in fallback hostname, got {hostname}"
    );
}
