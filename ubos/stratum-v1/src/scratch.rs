//! Typed binary-derivation slots.
//!
//! V1's wire form is hex-ASCII; the visitor wants binary. Each
//! `accept()` call hex-decodes the borrowed wire fields into this
//! struct and hands the visitor borrowed slices that point in here.
//! Reset (logically — the values are simply overwritten) at the
//! start of every notify dispatch.

/// Maximum bytes for `coinbase_1` / `coinbase_2` after hex decode.
/// Real Ocean notifies exceed the old 512-byte cap: a captured
/// production notify carries a 587-byte `coinb2` (`stratum-comm-sample`,
/// repo root) and block 953001's `coinb2` needs 667 bytes. 1024 gives
/// headroom above both; the 4096-byte wire buffer bounds the whole
/// notify line regardless.
pub const MAX_COINBASE_PART: usize = 1024;

/// Maximum merkle path length supported by V1 today.
pub const MAX_MERKLE_BRANCHES: usize = 16;

/// Maximum bytes for `extranonce_1` after hex decode.
pub const MAX_EXTRANONCE_1: usize = 8;

/// Storage for the binary form of one notify. Slices in
/// `MiningJob<'_>::merkle::Prepared` and the `prev_hash` field point
/// in here for the duration of the visitor call.
pub struct DecodeScratch {
    pub prev_hash: [u8; 32],
    pub target: [u8; 32],
    pub coinbase_1: [u8; MAX_COINBASE_PART],
    pub coinbase_1_len: usize,
    pub coinbase_2: [u8; MAX_COINBASE_PART],
    pub coinbase_2_len: usize,
    pub branches: [[u8; 32]; MAX_MERKLE_BRANCHES],
    pub branch_count: usize,
}

impl DecodeScratch {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_hash: [0; 32],
            target: [0; 32],
            coinbase_1: [0; MAX_COINBASE_PART],
            coinbase_1_len: 0,
            coinbase_2: [0; MAX_COINBASE_PART],
            coinbase_2_len: 0,
            branches: [[0; 32]; MAX_MERKLE_BRANCHES],
            branch_count: 0,
        }
    }
}

impl Default for DecodeScratch {
    fn default() -> Self {
        Self::new()
    }
}
