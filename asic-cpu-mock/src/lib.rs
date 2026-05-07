//! CPU-side mock of an ASIC, talking to `stratum-client` through
//! `AsicSession`.
//!
//! Polls the work cell, hashes a chunk of nonces, pushes solutions
//! back. Replaces today's `cpu-miner` integration.

#![no_std]
