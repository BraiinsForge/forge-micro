//! Tests for the dashboard asset pipeline.
//!
//! The HTTP handler itself is not exercised here because host-runner
//! has no edge-http server harness yet; add integration coverage there
//! when that gap closes. Until then, the invariants the handler
//! relies on — payload is a real gzip stream that decompresses to
//! well-formed HTML containing every source piece — are unit-tested
//! below.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can allocate format strings and decompress.
extern crate std;

use std::string::String;
use std::vec::Vec;

/// Decompress a gzipped asset into a UTF-8 string.
fn decompress(gz: &[u8]) -> String {
    let mut decoder = flate2::read::GzDecoder::new(gz);
    let mut buffer = Vec::new();
    // `gz` is the build-time output of a `GzEncoder` over a `String`, so the
    // stream is well-formed and decompresses to valid UTF-8.
    std::io::Read::read_to_end(&mut decoder, &mut buffer).expect("BUG: gunzip body");
    String::from_utf8(buffer).expect("BUG: payload utf8")
}

/// Decompressed payload must look like real HTML at both ends. Catches
/// a build.rs regression that emits the wrong file or truncates the
/// stream.
#[test]
fn index_payload_is_well_formed_html() {
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(
        html.starts_with("<!DOCTYPE html>"),
        "missing doctype: {:?}",
        &html[..html.len().min(40)]
    );
    assert!(
        html.trim_end().ends_with("</html>"),
        "missing closing </html>"
    );
}

/// Every template marker must be substituted. Catches a regression
/// where build.rs reads the wrong file, skips the replace pass, or
/// changes the marker spelling without updating the template.
#[test]
fn index_payload_has_no_unsubstituted_markers() {
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(!html.contains("<!--CSS-->"), "CSS marker not substituted");
    assert!(!html.contains("<!--JS-->"), "JS marker not substituted");
    assert!(
        !html.contains("<!--ICONS-->"),
        "icons marker not substituted"
    );
    assert!(
        !html.contains("{{BUILD_REV}}"),
        "build rev marker not substituted"
    );
    assert!(
        !html.contains("{{BUILD_TIME}}"),
        "build time marker not substituted"
    );
}

/// Each source piece must be semantically present in the payload.
/// esbuild minification mangles local identifiers but preserves
/// attribute values, CSS custom-property names, and string literals;
/// the anchors below are chosen so the assertions stay stable across
/// minifier upgrades.
#[test]
fn index_payload_contains_semantic_anchors() {
    let html = decompress(crate::INDEX_HTML_GZ);
    // HTML: id attributes survive minification.
    assert!(html.contains(r#"id="cHr""#), "missing chart id");
    assert!(html.contains(r#"id="cfgForm""#), "missing config form id");
    // CSS: custom-property names are preserved by esbuild.
    assert!(html.contains("--accent"), "missing CSS custom property");
    // JS: string literals are not mangled.
    assert!(html.contains("/api/info"), "missing /api/info literal");
    assert!(html.contains("/api/config"), "missing /api/config literal");
    // OTA card: id attribute and endpoint path must be present.
    assert!(html.contains(r#"id="fwCard""#), "missing firmware card id");
    assert!(
        html.contains("/api/system/OTA"),
        "missing OTA endpoint path"
    );
    // Events tab: section id, nav link id, and API path must be present.
    assert!(html.contains(r#"id="events""#), "missing events section id");
    assert!(
        html.contains(r#"id="nEvents""#),
        "missing events nav link id"
    );
    assert!(
        html.contains("/api/system/events"),
        "missing /api/system/events path"
    );
}

/// Logout must ship as a navigation form, not a JS-driven anchor. The
/// form lets the browser follow the `302` to `/login` in one
/// browser-paced request; the prior `fetch`-then-`location.href` anchor
/// fired three back-to-back requests at the server, which then served
/// one at a time. Asserting the markup guards against regressing to
/// that pattern.
#[test]
fn index_payload_logout_is_post_form() {
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(
        html.contains(r#"action="/logout""#),
        "logout is not a form posting to /logout"
    );
    assert!(
        html.contains(r#"method="post""#),
        "logout form does not POST"
    );
}

/// Loose upper bound on the gzipped dashboard payload. Catches a
/// minifier regression that silently no-ops, a source addition that
/// crosses the budget we care about, or a build.rs change that ships
/// uncompressed bytes. The bound sits above the 14 KiB TCP
/// initcwnd, so the index takes two RTTs on first load; the OTA
/// card is hidden on boards without an OTA partition so the extra
/// bytes are always loaded but not always displayed, and the Events
/// tab adds further markup on top. Bump deliberately when
/// intentionally adding features, not as a reflexive update.
#[test]
fn index_payload_size_within_budget() {
    const MAX_GZ_BYTES: usize = 19 * 1024;
    assert!(
        crate::INDEX_HTML_GZ.len() <= MAX_GZ_BYTES,
        "gz payload {} exceeds budget {}",
        crate::INDEX_HTML_GZ.len(),
        MAX_GZ_BYTES
    );
}

/// `INDEX_HTML_GZ` must contain a non-empty gzip stream. Names the
/// cheapest invariant precisely so a build.rs change that emits an
/// empty buffer or a non-gzip payload fails here rather than in the
/// browser with a `Content-Encoding: gzip` rejection.
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

/// Same shell-and-marker checks for the login page, with a tighter
/// size bound: login is much smaller and a regression that doubles
/// it is worth catching.
#[test]
fn login_payload_is_well_formed_and_within_budget() {
    const MAX_GZ_BYTES: usize = 3 * 1024;
    let html = decompress(crate::LOGIN_HTML_GZ);
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
    assert!(!html.contains("<!--CSS-->"));
    assert!(!html.contains("<!--JS-->"));
    assert!(!html.contains("<!--ICONS-->"));
    assert!(!html.contains("{{BUILD_REV}}"));
    assert!(!html.contains("{{BUILD_TIME}}"));
    assert!(html.contains(r#"action="/login""#), "missing login form");
    assert!(
        crate::LOGIN_HTML_GZ.len() <= MAX_GZ_BYTES,
        "gz payload {} exceeds budget {}",
        crate::LOGIN_HTML_GZ.len(),
        MAX_GZ_BYTES
    );
}

#[test]
fn login_asset_is_non_empty_gzip() {
    assert!(
        crate::LOGIN_HTML_GZ.len() >= 10,
        "gzip stream shorter than header"
    );
    assert_eq!(
        &crate::LOGIN_HTML_GZ[..2],
        &[0x1f, 0x8b],
        "missing gzip magic"
    );
}
