//! Render tests for the HTTP response builders.
//!
//! The api/info builder reads module-level atomics, so each test that
//! mutates them takes `STATE_LOCK` to serialize against other tests
//! running in parallel. We deliberately leave `EWMA_HASHRATE` and
//! `BUCKET_PRIMED` zeroed so `hashrate()` returns early without
//! touching `embassy_time::Instant::now()`, which has no driver in a
//! host test process.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

// `build_api_info_response` reaches `hashrate::hashrate`, which has a
// linkable reference to `embassy_time::Instant::now()`. No embassy-time
// driver is loaded in a host test binary, so satisfy the linker with
// stubs. The code path that calls `Instant::now()` is gated behind a
// non-zero `EWMA_HASHRATE`, which `reset_atomics` keeps at zero, so
// these stubs are never executed at runtime.
//
// `embassy-time-driver` declares these symbols as `extern "Rust"`, so
// matching that ABI is intentional even though clippy warns about it
// under `--all-targets`.
#[allow(clippy::no_mangle_with_rust_abi)]
#[unsafe(no_mangle)]
fn _embassy_time_now() -> u64 {
    panic!("embassy-time tick reached in host test; check EWMA_HASHRATE state")
}
#[allow(clippy::no_mangle_with_rust_abi)]
#[unsafe(no_mangle)]
fn _embassy_time_schedule_wake(_at: u64, _waker: &core::task::Waker) {
    panic!("embassy-time scheduler reached in host test")
}

use std::sync::Mutex;

use portable_atomic::Ordering;

use super::{build_api_info_response, build_index_response, INDEX_HTML};
use crate::consts::Http;
use ubos_core::fan_state::{FAN_PWM_PERCENT, FAN_RPM};
use crate::net::{LOCAL_IPV4, NTP_EPOCH_OFFSET, POOL_ALIVE_BITMAP, TEMPERATURE_C};

static STATE_LOCK: Mutex<()> = Mutex::new(());

fn body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("response has CRLFCRLF header/body separator")
}

fn reset_atomics() {
    LOCAL_IPV4.store(0, Ordering::Relaxed);
    NTP_EPOCH_OFFSET.store(0, Ordering::Relaxed);
    TEMPERATURE_C.store(0, Ordering::Relaxed);
    FAN_RPM.store(0, Ordering::Relaxed);
    FAN_PWM_PERCENT.store(u8::MAX, Ordering::Relaxed);
    POOL_ALIVE_BITMAP.store(0, Ordering::Relaxed);
}

/// Source HTML carries the pools panel. Asserted against the source
/// file rather than the response body because the body is gzipped on
/// the wire; the round-trip test below covers source → compressed →
/// decompressed equality, so adding "source contains X" closes the
/// chain without a second gunzip per check.
#[test]
fn dashboard_html_carries_pools_panel() {
    let document = include_str!("../index.html");
    assert!(
        document.contains("class=\"k\">Pools<"),
        "missing Pools panel heading"
    );
    for id in ["po1", "po2", "po3"] {
        let needle = std::format!("id=\"{id}\"");
        assert!(document.contains(&needle), "missing pool row {id}");
    }
    // Each pool row carries a diode dot and a status label span. We
    // count occurrences rather than scanning per-row substrings: the
    // panel introduces three of each, and the nav bar contributes one
    // extra `.dot` for the link-status indicator.
    assert_eq!(document.matches("class=\"dot\"").count(), 4);
    assert_eq!(document.matches("class=\"pt\"").count(), 3);
    assert_eq!(document.matches(">Dead<").count(), 3);
}

/// Round-trip: decompressing the response body must yield the source
/// HTML unchanged. Catches a build.rs regression that silently emits
/// a stream for some other input.
#[test]
fn index_response_decompresses_to_source_html() {
    let mut buffer = [0u8; Http::RESPONSE_BUFFER_SIZE];
    let length = build_index_response(&mut buffer);
    let response = &buffer[..length];

    let sep = b"\r\n\r\n";
    let body_offset = response
        .windows(sep.len())
        .position(|window| window == sep)
        .expect("response separator")
        + sep.len();

    let mut decoder = flate2::read::GzDecoder::new(&response[body_offset..]);
    let mut decompressed = std::vec::Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed).expect("gunzip body");

    let source = include_str!("../index.html");
    assert_eq!(
        core::str::from_utf8(&decompressed).expect("decompressed utf8"),
        source
    );
}

#[test]
fn api_info_renders_pool_bitmap() {
    let _lock = STATE_LOCK.lock().expect("state lock");
    reset_atomics();

    for bitmap in [0u8, 1, 5, 7] {
        POOL_ALIVE_BITMAP.store(bitmap, Ordering::Relaxed);
        let mut buffer = [0u8; Http::RESPONSE_BUFFER_SIZE];
        let length = build_api_info_response(&mut buffer, "test-board", 42);
        let response = core::str::from_utf8(&buffer[..length]).expect("utf8 api/info");

        let needle = std::format!("\"pools\":{bitmap},");
        assert!(
            body(response).contains(&needle),
            "bitmap {bitmap}: missing {needle} in {response}"
        );
    }
}

#[test]
fn api_info_renders_all_telemetry() {
    let _lock = STATE_LOCK.lock().expect("state lock");
    reset_atomics();
    TEMPERATURE_C.store(55, Ordering::Relaxed);
    FAN_RPM.store(2400, Ordering::Relaxed);
    FAN_PWM_PERCENT.store(45, Ordering::Relaxed);
    POOL_ALIVE_BITMAP.store(3, Ordering::Relaxed);

    let mut buffer = [0u8; Http::RESPONSE_BUFFER_SIZE];
    let length = build_api_info_response(&mut buffer, "test-board", 99);
    let response = core::str::from_utf8(&buffer[..length]).expect("utf8 api/info");

    assert!(response.starts_with("HTTP/1.0 200 OK\r\n"));
    let document = body(response);
    for needle in [
        "\"name\":\"test-board\"",
        "\"hashrate\":0",
        "\"uptime\":99",
        "\"temperature\":55",
        "\"fan_rpm\":2400",
        "\"fan_pwm\":45",
        "\"pools\":3",
        "\"ip\":\"\"",
        "\"time\":\"\"",
    ] {
        assert!(document.contains(needle), "missing {needle} in {document}");
    }
}

/// Page-size guard against silent truncation. `BufferWriter::write_bytes`
/// returns `Err(fmt::Error)` on overflow, but the call site discards
/// it through `let _ = ...`. The socket then ships a headers-only
/// response and the browser renders a blank page. There is no log
/// line, no panic, no metric — only "the page is broken". Demanding
/// the rendered body end with the full `INDEX_HTML` bytes catches
/// that before it ships.
///
/// The wire-budget side of the same risk is enforced at compile time
/// by the assertion in `consts.rs` that keeps `RESPONSE_BUFFER_SIZE`
/// below `INITCWND_BUDGET`, so a single-RTT first paint is structural.
#[test]
fn index_response_fits_initcwnd_budget() {
    // Use the same buffer size the firmware allocates so a truncation
    // inside `build_index_response` shows up the same way it would in
    // production. A larger scratch buffer here would mask the buffer
    // ceiling that the firmware actually has to live with.
    let mut buffer = [0u8; Http::RESPONSE_BUFFER_SIZE];
    let length = build_index_response(&mut buffer);
    let response = &buffer[..length];

    assert!(
        response.ends_with(INDEX_HTML),
        "index response truncated at {length} bytes; INDEX_HTML is {} bytes",
        INDEX_HTML.len()
    );
}

#[test]
fn api_info_fan_pwm_sentinel_renders_as_zero() {
    let _lock = STATE_LOCK.lock().expect("state lock");
    reset_atomics();
    // `FAN_PWM_PERCENT` uses `u8::MAX` to mean "no fan on this board";
    // the JSON path must report zero so the dashboard treats it as
    // "not available" rather than "100 %".
    FAN_PWM_PERCENT.store(u8::MAX, Ordering::Relaxed);

    let mut buffer = [0u8; Http::RESPONSE_BUFFER_SIZE];
    let length = build_api_info_response(&mut buffer, "test-board", 0);
    let response = core::str::from_utf8(&buffer[..length]).expect("utf8 api/info");

    assert!(body(response).contains("\"fan_pwm\":0"));
}
