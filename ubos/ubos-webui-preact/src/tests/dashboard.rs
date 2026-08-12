//! Tests for the dashboard asset pipeline.
//!
//! The HTTP handler itself is not exercised here (host-runner has no
//! edge-http server harness yet). Until then, the invariants the
//! handler relies on — each payload is a real Brotli stream that
//! decompresses to well-formed HTML carrying the app's API surface —
//! are unit-tested below. The Vite build inlines all JS and CSS into
//! one HTML file per page, so the anchors are chosen from string
//! literals that survive terser minification (API paths, form
//! attributes) rather than from mangled identifiers or scoped classes.

// The crate is `#![no_std]`; opt back into std for the test harness so
// we can allocate format strings and decompress.
extern crate std;

use std::string::String;
use std::vec::Vec;

/// Decompress a Brotli asset into a UTF-8 string.
fn decompress(br: &[u8]) -> String {
    let mut input = br;
    let mut buffer = Vec::new();
    brotli::BrotliDecompress(&mut input, &mut buffer).expect("BUG: brotli decode body");
    String::from_utf8(buffer).expect("BUG: payload utf8")
}

/// Decompressed payload must look like real HTML at both ends. Catches
/// a build.rs regression that emits the wrong file or truncates the
/// stream.
#[test]
fn index_payload_is_well_formed_html() {
    let html = decompress(crate::INDEX_HTML_BR);
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

/// Build-provenance placeholders must be substituted by the Vite
/// `define` step. A leftover token means the build env was not wired
/// through to the frontend build.
#[test]
fn index_payload_has_no_unsubstituted_build_markers() {
    let html = decompress(crate::INDEX_HTML_BR);
    assert!(!html.contains("__BUILD_REV__"), "build rev not injected");
    assert!(!html.contains("__BUILD_TIME__"), "build time not injected");
    assert!(
        !html.contains("{{BUILD_REV}}"),
        "stale template marker present"
    );
}

/// Each API surface the SPA depends on must be present as a string
/// literal in the inlined bundle. esbuild does not mangle string
/// literals, so these anchors stay stable across minifier upgrades.
#[test]
fn index_payload_contains_api_surface() {
    let html = decompress(crate::INDEX_HTML_BR);
    assert!(html.contains("/api/info"), "missing /api/info literal");
    assert!(html.contains("/api/config"), "missing /api/config literal");
    assert!(
        html.contains("/api/system/events"),
        "missing /api/system/events path"
    );
    assert!(
        html.contains("/api/system/resume"),
        "missing /api/system/resume action path"
    );
}

/// Logout must ship as a navigation form, not a JS-driven anchor, so
/// the browser follows the `302` to `/login` in one browser-paced
/// request. Preact compiles the markup to `jsx('form', { action:
/// '/logout', method: 'post' })`, so the payload carries the form's
/// `action`/`method` as prop literals rather than HTML attributes.
#[test]
fn index_payload_logout_is_post_form() {
    let html = decompress(crate::INDEX_HTML_BR);
    assert!(
        html.contains(r#"action:"/logout""#),
        "logout is not a form with action /logout"
    );
    assert!(html.contains(r#""post""#), "logout form does not POST");
}

/// Upper bound on the Brotli dashboard payload. The Preact runtime
/// plus the compiled dashboard/config/events components sit above the
/// vanilla hand-written UI, so the budget is larger here; it still
/// guards against a minifier no-op or an accidental ship of
/// uncompressed bytes. Bump deliberately when adding features.
#[test]
fn index_payload_size_within_budget() {
    const MAX_BR_BYTES: usize = 28 * 1024;
    assert!(
        crate::INDEX_HTML_BR.len() <= MAX_BR_BYTES,
        "gz payload {} exceeds budget {}",
        crate::INDEX_HTML_BR.len(),
        MAX_BR_BYTES
    );
}

/// `INDEX_HTML_BR` must contain a non-empty Brotli stream that
/// decompresses, so a build.rs change that emits an empty buffer or a
/// non-Brotli payload fails here rather than in the browser with a
/// `Content-Encoding: br` decode error. Brotli carries no magic-number
/// prefix, so the guard is a successful round-trip to the doctype.
#[test]
fn index_asset_is_non_empty_brotli() {
    assert!(!crate::INDEX_HTML_BR.is_empty(), "empty brotli stream");
    assert!(
        decompress(crate::INDEX_HTML_BR).starts_with("<!DOCTYPE html>"),
        "brotli payload did not decode to HTML"
    );
}

/// The login page is framework-free (no Preact runtime), so it stays
/// small: shell checks plus a tight size bound that a regression
/// re-introducing the runtime here would trip.
#[test]
fn login_payload_is_well_formed_and_within_budget() {
    const MAX_BR_BYTES: usize = 4 * 1024;
    let html = decompress(crate::LOGIN_HTML_BR);
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
    assert!(!html.contains("__BUILD_REV__"));
    assert!(html.contains(r#"action="/login""#), "missing login form");
    assert!(
        crate::LOGIN_HTML_BR.len() <= MAX_BR_BYTES,
        "gz payload {} exceeds budget {}",
        crate::LOGIN_HTML_BR.len(),
        MAX_BR_BYTES
    );
}

#[test]
fn login_asset_is_non_empty_brotli() {
    assert!(!crate::LOGIN_HTML_BR.is_empty(), "empty brotli stream");
    assert!(
        decompress(crate::LOGIN_HTML_BR).starts_with("<!DOCTYPE html>"),
        "brotli payload did not decode to HTML"
    );
}
