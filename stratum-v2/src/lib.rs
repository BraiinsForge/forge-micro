//! Stratum V2 adapter implementing `mining-protocol::MiningClient`.
//!
//! Wraps the external `micro-sv2` library (via the `ii-sv2` umbrella)
//! and exposes a protocol-agnostic mining client to `stratum-client`.

#![no_std]
