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

//! REST API handler for the firmware.
//!
//! Serves the [`PATH_PREFIX`] surface and the reference-miner-shaped read
//! endpoints under `/api/system/*`. The web-UI shell that shares the
//! listener lives in `ubos-webui`, and the transport, the router that
//! joins the two and the listener's own tuning live in `ubos-http`.
//!
//! `REST_API_SERVICE` advertises the surface via mDNS. Firmware includes
//! it in the service list passed to [`ubos_mdns::run_mdns`].
#![no_std]

/// The path prefix every route in this crate lives under.
///
/// `ubos_firmware::serve_all!` routes it to this handler, and the mDNS
/// entry below reports it to a machine client that discovers the board.
pub const PATH_PREFIX: &str = "/api/";

ubos_mdns::declare_service!(
    pub REST_API_SERVICE = ubos_mdns::ServiceEntry {
        service: "_ubos",
        port: ubos_http::consts::Http::PORT,
        txt_keys: &["path"],
        txt_values: &[PATH_PREFIX],
    }
);

pub mod hw_info;
pub mod ota;
pub mod rest;
pub mod schema;
pub mod system;

ubos_events::events! {
    user_paused()               info = "User paused mining",
    user_requested_start()      info = "User requested mining start",
    user_restart()              warn = "User-requested restart",
    user_cleared_thermal_halt() warn = "User cleared the dangerous-temperature halt",
}

pub use hw_info::{
    HW_STRING_LEN, HashboardIds, HwString, publish_bootloader_version,
    publish_fan_controller_version, publish_hashboard_id, publish_hashboard_ids,
    publish_nominal_hashrate, publish_product_name,
};
pub use ota::{
    OtaError, OtaTarget, OtaWritePhase, ResetReason, publish_reset_diag, register_ota_target,
};
pub use rest::{
    RestApiService, run_ota_session_watchdog, run_sync_credentials, seed_api_credentials,
};

#[cfg(test)]
mod test_time_stubs {
    // `rest` and `system` call `embassy_time::Instant::now()` directly,
    // and `ubos_auth` does so transitively from this crate's tests.
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
