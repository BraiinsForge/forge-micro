//! HTTP API surface for the firmware web UI and external clients.
//!
//! This crate owns the request handlers; transport primitives live in
//! `ubos-http`. It serves the dashboard's four legacy endpoints (`/`,
//! `/logo.svg`, `/api/info`, `/api/config`) and the reference-miner-shaped
//! read endpoints under `/api/system/*`. The system endpoints are
//! dispatched by `system::dispatch` before the legacy match so the
//! SPA's existing `/api/info` poll keeps working unchanged.
//!
//! Only `/`, `/login`, `/logout`, and `/logo.svg` are anonymous; every
//! `/api/*` endpoint (including `/api/info` and `/api/config`) requires
//! a valid session cookie or `Authorization: Basic` header.
//!
//! `HTTP_API_SERVICE` is the mDNS advertisement for the HTTP dashboard
//! endpoint; `REST_API_SERVICE` advertises the REST listener on port
//! 8080. Firmware includes both in the service list passed to
//! [`ubos_mdns::run_mdns`].
#![no_std]

ubos_mdns::declare_service!(
    pub HTTP_API_SERVICE = ubos_mdns::ServiceEntry {
        service: ubos_core::consts::Mdns::HTTP_SERVICE,
        port: ubos_core::consts::Http::PORT,
        txt_keys: &["path"],
        txt_values: &[ubos_core::consts::Mdns::HTTP_PATH],
    }
);

ubos_mdns::declare_service!(
    pub REST_API_SERVICE = ubos_mdns::ServiceEntry {
        service: ubos_core::consts::Mdns::UBOS_API_SERVICE,
        port: ubos_core::consts::Api::PORT,
        txt_keys: &["path"],
        txt_values: &[ubos_core::consts::Mdns::UBOS_API_PATH],
    }
);

pub mod dashboard;
pub mod rest;
pub mod schema;
pub mod system;

pub use dashboard::run;
pub use rest::run_rest_api;
