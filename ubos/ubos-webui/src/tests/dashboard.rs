//! Tests for the dashboard request handler and its asset pipeline.
//!
//! The handler dispatch is not exercised here because host-runner has
//! no edge-http server harness yet; add integration coverage there
//! when that gap closes. Until then, the pure invariants the handler
//! relies on — asset round-trip and asset non-emptiness — are
//! unit-tested below.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

/// Round-trip: decompressing the asset bytes must yield the source HTML
/// unchanged. Catches a build.rs regression that silently emits a
/// stream for some other input.
#[test]
fn index_asset_decompresses_to_source_html() {
    let mut decoder = flate2::read::GzDecoder::new(crate::INDEX_HTML_GZ);
    let mut decompressed = std::vec::Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed).expect("gunzip body");

    let source = crate::INDEX_HTML_SOURCE;
    assert_eq!(
        core::str::from_utf8(&decompressed).expect("decompressed utf8"),
        source
    );
}

/// `INDEX_HTML_GZ` must contain a non-empty gzip stream. A build.rs
/// regression that emits an empty buffer or a non-gzip payload would
/// leave the GET `/` handler serving zero bytes with a `Content-Encoding:
/// gzip` header, which the browser would reject. The decompression
/// round-trip above already catches semantic damage; this test guards
/// the cheaper invariants so the failure mode is named precisely.
#[test]
fn index_asset_is_non_empty_gzip() {
    assert!(
        crate::INDEX_HTML_GZ.len() >= 10,
        "gzip stream shorter than header"
    );
    assert_eq!(
        &crate::INDEX_HTML_GZ[..2],
        &[0x1f, 0x8b],
        "missing gzip magic"
    );
}
