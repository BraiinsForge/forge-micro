//! Stratum V2 adapter implementing `mining-protocol::MiningClient`.
//!
//! Wraps the external `micro-sv2` library (via the `ii-sv2`
//! umbrella) and exposes a protocol-agnostic mining client to
//! `stratum-client`.

#![no_std]

mod client;
mod config;

pub use client::{StratumV2Client, ENCODE_BUFFER_BYTES, WIRE_BUFFER_BYTES};
pub use config::Sv2Config;
