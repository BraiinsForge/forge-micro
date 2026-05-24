//! Tests for the dashboard request handler and its asset pipeline.
//!
//! Currently a single round-trip test against the gzipped index asset.
//! The handler itself is not exercised here because host-runner has no
//! edge-http server harness yet; add integration coverage there when
//! that gap closes.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

// The `session` and `csrf` modules call `embassy_time::Instant::now()`
// during their static initialization, and `embassy-time-driver`
// declares the driver hooks as `extern "Rust"`. No driver is linked
// into the host test binary, so satisfy the linker with stubs that
// return a fixed monotonic tick of zero.
#[allow(clippy::no_mangle_with_rust_abi)]
#[unsafe(no_mangle)]
fn _embassy_time_now() -> u64 {
    0
}
#[allow(clippy::no_mangle_with_rust_abi)]
#[unsafe(no_mangle)]
fn _embassy_time_schedule_wake(_at: u64, _waker: &core::task::Waker) {
    // No scheduler in the host binary; the test paths do not block on
    // timers so leaving this empty is safe.
}

/// Round-trip: decompressing the asset bytes must yield the source HTML
/// unchanged. Catches a build.rs regression that silently emits a
/// stream for some other input.
#[test]
fn index_asset_decompresses_to_source_html() {
    use ubos_webui::INDEX_HTML_GZ;

    let mut decoder = flate2::read::GzDecoder::new(INDEX_HTML_GZ);
    let mut decompressed = std::vec::Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed).expect("gunzip body");

    let source = ubos_webui::INDEX_HTML_SOURCE;
    assert_eq!(
        core::str::from_utf8(&decompressed).expect("decompressed utf8"),
        source
    );
}

// --- M7: validate_board_name ---

/// Board names with no special characters must pass the check.
#[test]
fn validate_board_name_accepts_safe_names() {
    use crate::dashboard::validate_board_name;
    assert!(validate_board_name("BMM Adapter W5500"));
    assert!(validate_board_name("host-runner"));
    assert!(validate_board_name(""));
    assert!(validate_board_name("abc123 !@#$%^&*()"));
}

/// A board name containing a double-quote must be rejected.
#[test]
fn validate_board_name_rejects_double_quote() {
    use crate::dashboard::validate_board_name;
    assert!(!validate_board_name("bad\"name"));
    assert!(!validate_board_name("\"starts"));
    assert!(!validate_board_name("ends\""));
}

/// A board name containing a backslash must be rejected.
#[test]
fn validate_board_name_rejects_backslash() {
    use crate::dashboard::validate_board_name;
    assert!(!validate_board_name("bad\\name"));
    assert!(!validate_board_name("\\starts"));
    assert!(!validate_board_name("ends\\"));
}
