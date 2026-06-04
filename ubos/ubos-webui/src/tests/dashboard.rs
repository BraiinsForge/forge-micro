//! Tests for the dashboard request handler and its asset pipeline.
//!
//! Currently a single round-trip test against the gzipped index asset.
//! The handler itself is not exercised here because host-runner has no
//! edge-http server harness yet; add integration coverage there when
//! that gap closes.

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
