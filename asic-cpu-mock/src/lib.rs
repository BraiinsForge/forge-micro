//! CPU-side mock of an ASIC, talking to `stratum-client` through
//! `AsicSession`.
//!
//! Polls the work cell, hashes a chunk of nonces, pushes solutions
//! back. Replaces today's `cpu-miner` integration.

#![no_std]

mod hash;
mod header;
mod run;

#[cfg(test)]
#[path = "tests/run.rs"]
mod tests;

pub use hash::{hash_meets_target, sha256d};
pub use header::{build_header, HEADER_BYTES};
pub use run::{nonce_meets_target, run_asic_cpu, HASHES_PER_CHUNK};
