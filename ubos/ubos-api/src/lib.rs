//! REST API listener on port 8080 for the firmware.
//!
//! Serves the `/api/*` surface and the reference-miner-shaped read endpoints
//! under `/api/system/*`. The web-UI shell on port 80 lives in
//! `ubos-webui`. Transport primitives live in `ubos-http`.
//!
//! `REST_API_SERVICE` advertises the REST listener on port 8080 via
//! mDNS. Firmware includes it in the service list passed to
//! [`ubos_mdns::run_mdns`].
#![no_std]

ubos_mdns::declare_service!(
    pub REST_API_SERVICE = ubos_mdns::ServiceEntry {
        service: "_ubos",
        port: ubos_core::consts::Api::PORT,
        txt_keys: &["path"],
        txt_values: &["/api/"],
    }
);

pub mod ota;
pub mod rest;
pub mod schema;
pub mod system;

ubos_events::events! {
    user_paused()       info = "User paused mining",
    user_resumed()      info = "User resumed mining",
    user_restart()      warn = "User-requested restart",
}

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
