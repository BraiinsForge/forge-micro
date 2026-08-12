//! Web-UI listener on port 80 (Preact build).
//!
//! Owns the dashboard SPA HTML, login form, and logout handler. The
//! REST API surface (`/api/*`) lives in `ubos-api` on port 8080. mDNS
//! announces `_http._tcp` on port 80 via `WEBUI_SERVICE`.
//!
//! This crate is a drop-in sibling of `ubos-webui`: the port-80
//! `DashboardService` is byte-for-byte the same, but the frontend is
//! built from a Preact + Vite project (see `web/`, `build.rs`) instead
//! of hand-written HTML/CSS/JS. The serving contract is unchanged: the
//! build emits `index.html.br` and `login.html.br` into `OUT_DIR`,
//! served with `Content-Encoding: br`.
#![no_std]

/// Brotli-compressed dashboard payload. Built from the Preact project's
/// `index` entry by the crate's `build.rs` (Vite single-file build, then
/// Brotli quality 11).
pub const INDEX_HTML_BR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.br"));

/// Brotli-compressed login-page payload. Built from the Preact project's
/// `login` entry by the crate's `build.rs` (Vite single-file build, then
/// Brotli quality 11).
pub const LOGIN_HTML_BR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/login.html.br"));

/// Brotli-compressed Braiins wordmark SVG served at `/logo.svg`.
/// Compressed by the crate's `build.rs` from `src/logo.svg` and served
/// with `Content-Encoding: br`.
pub const LOGO_SVG_BR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/logo.svg.br"));

ubos_mdns::declare_service!(
    pub WEBUI_SERVICE = ubos_mdns::ServiceEntry {
        service: "_http",
        port: consts::Http::PORT,
        txt_keys: &["path"],
        txt_values: &["/"],
    }
);

pub mod consts;
pub mod dashboard;
pub use dashboard::{DashboardService, seed_webui_credentials};

#[cfg(test)]
mod test_time_stubs {
    // `ubos_auth::session` and `ubos_auth::csrf` call
    // `embassy_time::Instant::now()` transitively via `DashboardService::handle`.
    // `embassy-time-driver` declares the hook as `extern "Rust"`; no
    // hardware driver is linked into the host test binary, so satisfy
    // the linker with stubs that return a fixed tick of zero.
    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_now() -> u64 {
        0
    }

    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_schedule_wake(_at: u64, _waker: &core::task::Waker) {
        // No scheduler in the host binary; test paths do not block on
        // timers so leaving this empty is safe.
    }
}
