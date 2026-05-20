//! Stratum V1 wire layer: framing (`line_buffer`), message types
//! (`messages`), parser (`parse`), and the JSON-RPC envelope (`rpc`).
//!
//! Usable standalone — `examples/probe.rs` connects to a real pool
//! using only types from this module. Higher-level integration that
//! plugs into `mining-protocol::MiningClient` lives in
//! `crate::client` (added in phase 5).

pub mod line_buffer;
pub mod messages;
pub mod parse;
pub mod rpc;
