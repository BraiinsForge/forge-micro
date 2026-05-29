//! Observability for the firmware framework.
//!
//! `metrics` builds the Prometheus text-format body and serves it via
//! an edge-http `Handler` driven by a dedicated accept loop on the
//! Prometheus port. `hashrate` owns the EWMA accumulator that the
//! `ubos-mining` firmware path and the host-runner CPU mock push work
//! into. `cpu_load` samples per-core idle ticks and exposes a
//! busy-ratio gauge.
//!
//! `PROMETHEUS_SERVICE` is the mDNS advertisement for the Prometheus
//! endpoint; firmware that advertises this crate includes it in the
//! service list it passes to [`ubos_mdns::run_mdns`].
#![no_std]

ubos_mdns::declare_service!(
    pub PROMETHEUS_SERVICE = ubos_mdns::ServiceEntry {
        service: ubos_core::consts::Mdns::PROMETHEUS_SERVICE,
        port: ubos_core::consts::Metrics::PORT,
        txt_keys: &["path"],
        txt_values: &[ubos_core::consts::Mdns::METRICS_PATH],
    }
);

pub mod cpu_load;
pub mod hashrate;
pub mod metrics;
