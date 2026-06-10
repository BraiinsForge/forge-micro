//! Tests for `system::route` — method enforcement and path dispatch.
//!
//! Every test exercises the pure `route()` function with concrete method
//! and path strings. No `Connection`, executor, or TCP socket is involved.
//! The `route()` function returns a `DispatchOutcome` that encodes the full
//! routing decision; tests assert on that value directly.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness.
extern crate std;

use crate::system::{route, DispatchOutcome, Route};

/// Each row: (allowed-method, path, expected-Route-on-match).
///
/// Adding a row covers both the accepted-method case and the
/// other-method rejection. A new path with no row is a coverage gap.
const ROUTES: &[(&str, &str, Route)] = &[
    ("GET", "/api/system/info", Route::SystemInfo),
    ("GET", "/api/system/asic", Route::SystemAsic),
    ("GET", "/api/system/thermal", Route::SystemThermal),
    ("GET", "/api/system/cooling", Route::SystemCooling),
    ("GET", "/api/system/statistics", Route::SystemStatistics),
    ("GET", "/api/system/scoreboard", Route::SystemScoreboard),
    ("GET", "/api/system/logs", Route::SystemLogs),
    ("GET", "/api/system/wifi/scan", Route::SystemWifiScan),
    ("POST", "/api/system/OTA", Route::SystemOta),
    ("POST", "/api/system/OTAWWW", Route::SystemOtaWww),
    ("POST", "/api/system/pause", Route::PauseAck),
    ("POST", "/api/system/resume", Route::ResumeAck),
    ("POST", "/api/system/restart", Route::RestartAck),
    ("POST", "/api/system/identify", Route::IdentifyAck),
    (
        "POST",
        "/api/system/blockFound/dismiss",
        Route::BlockFoundDismissAck,
    ),
    ("POST", "/api/system/factory_reset", Route::FactoryReset),
    ("PATCH", "/api/system", Route::PatchSystem),
];

#[test]
fn route_table_handles_matching_method_and_rejects_others() {
    // Probe each row with its own method and with a representative
    // alternative. The alternative is chosen so that for any allowed
    // method the probe lands on a different verb, which exercises the
    // 405 path with `allowed:` matching the row's method.
    for (allowed, path, expected) in ROUTES {
        assert_eq!(
            route(allowed, path),
            DispatchOutcome::Handled(*expected),
            "{allowed} {path} should be Handled",
        );
        let wrong = match *allowed {
            "GET" => "POST",
            "POST" | "PATCH" => "GET",
            other => panic!("test table needs an alternative for method {other}"),
        };
        assert_eq!(
            route(wrong, path),
            DispatchOutcome::MethodNotAllowed { allowed },
            "{wrong} {path} should be MethodNotAllowed (allowed={allowed})",
        );
    }
}

#[test]
fn route_unknown_system_subpath_not_found() {
    assert_eq!(
        route("GET", "/api/system/unknown"),
        DispatchOutcome::NotFound
    );
}

#[test]
fn route_api_system_trailing_slash_not_found() {
    assert_eq!(route("GET", "/api/system/"), DispatchOutcome::NotFound);
}

#[test]
fn route_root_not_found() {
    assert_eq!(route("GET", "/"), DispatchOutcome::NotFound);
}

#[test]
fn route_api_info_not_found() {
    assert_eq!(route("GET", "/api/info"), DispatchOutcome::NotFound);
}
