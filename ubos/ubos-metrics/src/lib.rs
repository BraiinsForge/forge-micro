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
        service: "_prometheus",
        port: consts::Metrics::PORT,
        txt_keys: &["path"],
        txt_values: &["/metrics"],
    }
);

pub mod catalog;
pub mod chain;
pub mod consts;
pub mod cpu_load;
mod freshness;
pub mod hashrate;
pub mod metrics;
mod name;
mod store;
pub mod value;

pub use metrics::Metric;
pub use name::{Name, Unit};
