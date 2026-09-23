// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Tests for the dashboard asset pipeline.
//!
//! The HTTP handler itself is not exercised here (host-runner has no
//! edge-http server harness yet). Until then, the invariants the
//! handler relies on — each payload is a real gzip stream that
//! decompresses to well-formed HTML carrying the app's API surface —
//! are unit-tested below. The Vite build inlines all JS and CSS into
//! one HTML file per page, so the anchors are chosen from string
//! literals that survive terser minification (API paths, form
//! attributes) rather than from mangled identifiers or scoped classes.

// The crate is `#![no_std]`; opt back into std for the test harness so
// we can allocate format strings and decompress.
extern crate std;

use std::io::Read;
use std::string::String;

/// The brand tokens `build.rs` stamps, which no shipped page may still
/// carry. Both pages carry all four, so one list serves both tests.
const BRAND_TOKENS: [&str; 4] = [
    "@UBOS_BRAND_NAME@",
    "@UBOS_BRAND_ACCENT@",
    "@UBOS_BRAND_ACCENT_HI@",
    "@UBOS_BRAND_ACCENT_TEXT@",
];

/// The brand name the build resolved, written to `OUT_DIR` by build.rs
/// from the same `brand.json` it stamped the pages with.
const BRAND_NAME: &str = include_str!(concat!(env!("OUT_DIR"), "/brand-name"));

/// Decompress a gzipped asset into a UTF-8 string.
fn decompress(gz: &[u8]) -> String {
    let mut html = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut html)
        .expect("BUG: gzip decode body");
    html
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

/// Build-provenance and brand markers must be gone from the embedded
/// page: the bundler's `define` step and the HTML template replace the
/// source identifiers with the stamp tokens, and build.rs replaces the
/// tokens with the revision, the time and the brand. A leftover of
/// either kind means one of the two steps did not run.
#[test]
fn index_payload_has_no_unsubstituted_build_markers() {
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(!html.contains("__BUILD_REV__"), "build rev not injected");
    assert!(!html.contains("__BUILD_TIME__"), "build time not injected");
    assert!(!html.contains("__BRAND_NAME__"), "brand name not injected");
    assert!(
        !html.contains("@UBOS_WEBUI_BUILD_REV@"),
        "build rev not stamped"
    );
    assert!(
        !html.contains("@UBOS_WEBUI_BUILD_TIME@"),
        "build time not stamped"
    );
    for token in BRAND_TOKENS {
        assert!(!html.contains(token), "{token} not stamped");
    }
    assert!(
        html.contains(BRAND_NAME),
        "page does not carry the brand name {BRAND_NAME}"
    );
    assert!(
        !html.contains("{{BUILD_REV}}"),
        "stale template marker present"
    );
}

/// Each API surface the SPA depends on must be present as a string
/// literal in the inlined bundle. SWC does not mangle string
/// literals, so these anchors stay stable across minifier upgrades.
#[test]
fn index_payload_contains_api_surface() {
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(html.contains("/api/info"), "missing /api/info literal");
    assert!(html.contains("/api/config"), "missing /api/config literal");
    // The minifier renames the other props, so the check reads only the
    // power field's own props, up to the brace that closes them.
    let (_, power) = html
        .split_once(r#"label:"Input power""#)
        .expect("the power field is in the bundle");
    let (field, _) = power
        .split_once('}')
        .expect("the power field's props close");
    assert!(
        field.contains(r#"min:"0""#),
        "power target must accept the zero safe-boot value"
    );
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
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(
        html.contains(r#"action:"/logout""#),
        "logout is not a form with action /logout"
    );
    assert!(html.contains(r#""post""#), "logout form does not POST");
}

/// Upper bound on the gzipped dashboard payload. The Preact runtime
/// plus the compiled dashboard/config/events components sit above the
/// vanilla hand-written UI, so the budget is larger here; it still
/// guards against a minifier no-op or an accidental ship of
/// uncompressed bytes. Bump deliberately when adding features.
#[test]
fn index_payload_size_within_budget() {
    const MAX_GZ_BYTES: usize = 28 * 1024;
    assert!(
        crate::INDEX_HTML_GZ.len() <= MAX_GZ_BYTES,
        "gz payload {} exceeds budget {}",
        crate::INDEX_HTML_GZ.len(),
        MAX_GZ_BYTES
    );
}

/// `INDEX_HTML_GZ` must contain a non-empty gzip stream that
/// decompresses, so a build.rs change that emits an empty buffer or a
/// non-gzip payload fails here rather than in the browser with a
/// `Content-Encoding: gzip` decode error. The guard is a successful
/// round-trip to the doctype rather than a magic-number check.
#[test]
fn index_asset_is_non_empty_gzip() {
    assert!(!crate::INDEX_HTML_GZ.is_empty(), "empty gzip stream");
    assert!(
        decompress(crate::INDEX_HTML_GZ).starts_with("<!DOCTYPE html>"),
        "gzip payload did not decode to HTML"
    );
}

/// The login page is framework-free (no Preact runtime), so it stays
/// small: shell checks plus a tight size bound that a regression
/// re-introducing the runtime here would trip.
#[test]
fn login_payload_is_well_formed_and_within_budget() {
    const MAX_GZ_BYTES: usize = 4 * 1024;
    let html = decompress(crate::LOGIN_HTML_GZ);
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
    assert!(!html.contains("__BUILD_REV__"));
    assert!(!html.contains("@UBOS_WEBUI_BUILD_REV@"));
    for token in BRAND_TOKENS {
        assert!(!html.contains(token), "{token} not stamped");
    }
    assert!(
        html.contains(BRAND_NAME),
        "page does not carry the brand name {BRAND_NAME}"
    );
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
    assert!(!crate::LOGIN_HTML_GZ.is_empty(), "empty gzip stream");
    assert!(
        decompress(crate::LOGIN_HTML_GZ).starts_with("<!DOCTYPE html>"),
        "gzip payload did not decode to HTML"
    );
}
