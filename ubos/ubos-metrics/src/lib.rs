//! Observability for the firmware framework.
//!
//! `metrics` runs the TCP server that serves the Prometheus text
//! format on a board-configurable port. `hashrate` owns the EWMA
//! accumulator that the MMM firmware path and the host-runner CPU
//! mock push work into. `cpu_load` samples per-core idle ticks and
//! exposes a busy-ratio gauge. `http` serves the human-readable
//! dashboard endpoints (HTML, JSON telemetry, SVG logo) and the
//! accept loop that drives them.
#![no_std]

pub mod cpu_load;
pub mod hashrate;
pub mod http;
pub mod metrics;
