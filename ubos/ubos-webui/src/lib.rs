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

//! Web-UI handler for the firmware (Preact build).
//!
//! Owns the dashboard SPA HTML, login form, and logout handler. The REST
//! API surface (`/api/*`) shares the listener and lives in `ubos-api`,
//! and the listener's own tuning lives in `ubos-http`. mDNS announces
//! `_http._tcp` via `WEBUI_SERVICE`.
#![no_std]

/// Gzipped dashboard payload, built by `build.rs` from the Preact
/// project's `index` entry.
pub const INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

/// Gzipped login-page payload, built by `build.rs` from the Preact
/// project's `login` entry.
pub const LOGIN_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/login.html.gz"));

/// Gzipped wordmark SVG of the dashboard, served at
/// `/dashboard-logo.svg` and taken from the brand directory the tree
/// declares as `UBOS_BRAND`.
pub const DASHBOARD_LOGO_SVG_GZ: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/dashboard-logo.svg.gz"));

/// Gzipped wordmark SVG of the login page, served at `/login-logo.svg`
/// and taken from the same brand directory.
pub const LOGIN_LOGO_SVG_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/login-logo.svg.gz"));

ubos_mdns::declare_service!(
    pub WEBUI_SERVICE = ubos_mdns::ServiceEntry {
        service: "_http",
        port: ubos_http::consts::Http::PORT,
        txt_keys: &["path"],
        txt_values: &["/"],
    }
);

pub mod dashboard;
pub use dashboard::{DashboardService, seed_webui_credentials};

#[cfg(test)]
mod test_time_stubs {
    // `DashboardService::handle` reaches `embassy_time::Instant::now()`
    // through `ubos_auth`, and no hardware time driver is linked into the
    // host test binary, so these stubs satisfy the linker.
    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_now() -> u64 {
        0
    }

    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_schedule_wake(_at: u64, _waker: &core::task::Waker) {
        // No test path blocks on a timer, so there is nothing to wake.
    }
}
