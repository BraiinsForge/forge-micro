//! Bundled web UI assets served by `ubos-api`.
//!
//! Owns the dashboard's HTML source, the wordmark SVG, and the
//! `build.rs` step that gzip-compresses the HTML at compile time
//! so the firmware ships the page with `Content-Encoding: gzip`.
//! Consumers receive the byte slices as `pub const`s and embed them
//! into HTTP responses; this crate has no runtime code.
#![no_std]

/// Gzipped dashboard payload produced from `src/index.html` by
/// the crate's `build.rs`.
pub const INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

/// Uncompressed source HTML, exposed so consumer tests can assert
/// round-trip equality against the gzipped form without reaching
/// for a cross-crate relative path.
pub const INDEX_HTML_SOURCE: &str = include_str!("index.html");

/// Gzipped login-page payload produced from `src/login.html` by
/// the crate's `build.rs`.
pub const LOGIN_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/login.html.gz"));

/// Braiins wordmark SVG served at `/logo.svg`.
pub const LOGO_SVG: &[u8] = include_bytes!("logo.svg");
