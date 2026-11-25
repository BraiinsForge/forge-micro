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
//! The HTTP handler itself is not exercised here; what is tested is the
//! invariant it relies on, that each payload is a real gzip stream which
//! decompresses to well-formed HTML carrying the app's API surface. The
//! anchors are string literals that survive minification, never mangled
//! identifiers or scoped classes.

// The crate is `#![no_std]`, and the harness needs allocation.
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

fn decompress(gz: &[u8]) -> String {
    let mut html = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut html)
        .expect("BUG: gzip decode body");
    html
}

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

#[test]
fn index_payload_has_no_unsubstituted_build_markers() {
    // A source identifier left behind means the bundler did not
    // substitute; a stamp token left behind means `build.rs` did not.
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(!html.contains("__BRAND_NAME__"), "brand name not injected");
    for token in BRAND_TOKENS {
        assert!(!html.contains(token), "{token} not stamped");
    }
    assert!(
        html.contains(BRAND_NAME),
        "page does not carry the brand name {BRAND_NAME}"
    );
}

#[test]
fn index_payload_contains_api_surface() {
    let html = decompress(crate::INDEX_HTML_GZ);
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

#[test]
fn index_payload_advertises_the_pubkey_in_the_pool_url() {
    let html = decompress(crate::INDEX_HTML_GZ);
    // The key is edited inside the pool URL rather than in a field of its
    // own, so the placeholder is what tells the operator it belongs there.
    assert!(
        html.contains("stratum2+tcp://host:port/pubkey"),
        "pool URL placeholder does not advertise the pubkey path"
    );
    // The anchor is the old rejection message, not the word `base64`:
    // Basic auth encodes its credentials that way, so the bare word lives
    // in the bundle for reasons of its own.
    assert!(
        !html.contains("32 bytes of base64"),
        "stale base64 pubkey rejection message still present"
    );
}

// Logout ships as a navigation form, not a JS-driven anchor, so the
// browser follows the `302` to `/login` in one browser-paced request.
// Preact compiles the markup to `jsx('form', { action: '/logout', method:
// 'post' })`, so the payload carries the form's `action` and `method` as
// prop literals rather than HTML attributes.
#[test]
fn index_payload_logout_is_post_form() {
    // Preact compiles the markup to a `jsx` call, so the form's action
    // and method ship as prop literals and not as HTML attributes.
    let html = decompress(crate::INDEX_HTML_GZ);
    assert!(
        html.contains(r#"action:"/logout""#),
        "logout is not a form with action /logout"
    );
    assert!(html.contains(r#""post""#), "logout form does not POST");
}

#[test]
fn index_payload_size_within_budget() {
    // The bound guards against a minifier no-op or an accidental ship of
    // uncompressed bytes.
    const MAX_GZ_BYTES: usize = 28 * 1024;
    assert!(
        crate::INDEX_HTML_GZ.len() <= MAX_GZ_BYTES,
        "gz payload {} exceeds budget {}",
        crate::INDEX_HTML_GZ.len(),
        MAX_GZ_BYTES
    );
}

#[test]
fn index_asset_is_non_empty_gzip() {
    assert!(!crate::INDEX_HTML_GZ.is_empty(), "empty gzip stream");
    assert!(
        decompress(crate::INDEX_HTML_GZ).starts_with("<!DOCTYPE html>"),
        "gzip payload did not decode to HTML"
    );
}

#[test]
fn login_payload_is_well_formed_and_within_budget() {
    // The login page carries no framework, so a regression that pulls the
    // runtime back into it trips this bound.
    const MAX_GZ_BYTES: usize = 4 * 1024;
    let html = decompress(crate::LOGIN_HTML_GZ);
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
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
