//! Stratum V2 adapter implementing `mining-protocol::MiningClient`.
//!
//! Wraps the external `micro-sv2` library (via the `ii-sv2`
//! umbrella) and exposes a protocol-agnostic mining client to
//! `stratum-client`.

#![no_std]

mod auth;
mod client;
mod config;
mod state;

pub use client::{ENCODE_BUFFER_BYTES, StratumV2Client, WIRE_BUFFER_BYTES};
pub use config::Sv2Config;
