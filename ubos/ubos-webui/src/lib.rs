//! Web-UI listener on port 80.
//!
//! Owns the dashboard SPA HTML, login form, and logout handler. The
//! REST API surface (`/api/*`) lives in `ubos-api` on port 8080. mDNS
//! announces `_http._tcp` on port 80 via `WEBUI_SERVICE`.
#![no_std]

/// Gzipped dashboard payload composed from `src/index.html`,
/// `src/index.css`, `src/index.js`, and `src/common.css` by the
/// crate's `build.rs`.
pub const INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

/// Gzipped login-page payload composed from `src/login.html`,
/// `src/login.css`, `src/login.js`, and `src/common.css` by the
/// crate's `build.rs`.
pub const LOGIN_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/login.html.gz"));

/// Braiins wordmark SVG served at `/logo.svg`.
pub const LOGO_SVG: &[u8] = include_bytes!("logo.svg");

ubos_mdns::declare_service!(
    pub WEBUI_SERVICE = ubos_mdns::ServiceEntry {
        service: "_http",
        port: ubos_core::consts::Http::PORT,
        txt_keys: &["path"],
        txt_values: &["/"],
    }
);

pub mod dashboard;
pub use dashboard::run;

#[cfg(test)]
mod test_time_stubs {
    // `ubos_auth::session` and `ubos_auth::csrf` call
    // `embassy_time::Instant::now()` transitively via `dashboard::run`.
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
