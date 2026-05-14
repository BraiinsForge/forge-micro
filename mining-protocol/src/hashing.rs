//! Bitcoin header construction, double-SHA256, and target comparison.
//!
//! Pure helpers used by the share-verification paths in mmm (chip-driver)
//! and any future on-MCU verifier. Lifted out of the retired `cpu-miner`
//! crate so callers do not pay for the CPU-mining loop just to validate
//! shares.

use sha2::{Digest, Sha256};

#[cfg(test)]
#[path = "tests/hashing.rs"]
mod tests;

/// Build the 80-byte Bitcoin block header.
///
/// `version`, `ntime`, and `nbits` serialise little-endian; `prev_hash`
/// has each 4-byte word reversed in place to undo ckpool's `swap_256`
/// convention; `merkle_root` is already in internal byte order from a
/// local double-SHA256 so it goes in raw. The caller converts stratum
/// big-endian hex to `u32` once during notify parsing so the hot path
/// only performs byte-layout work.
#[must_use]
pub fn build_header(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 80] {
    let mut header = [0u8; 80];

    header[0..4].copy_from_slice(&version.to_le_bytes());

    for word_index in 0..8 {
        let source = &prev_hash[word_index * 4..word_index * 4 + 4];
        let dest = &mut header[4 + word_index * 4..4 + word_index * 4 + 4];
        dest[0] = source[3];
        dest[1] = source[2];
        dest[2] = source[1];
        dest[3] = source[0];
    }

    header[36..68].copy_from_slice(&merkle_root);
    header[68..72].copy_from_slice(&ntime.to_le_bytes());
    header[72..76].copy_from_slice(&nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

/// Double-SHA256 of the 80-byte block header produced by `build_header`.
#[must_use]
pub fn header_hash(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 32] {
    let header = build_header(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let mut out = [0u8; 32];
    double_sha256(&header, &mut out);
    out
}

/// Return `true` when `hash` (little-endian 256-bit integer) is less
/// than or equal to `target`. Exact byte-by-byte compare from MSB down.
#[must_use]
pub fn hash_meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for index in (0..32).rev() {
        if hash[index] < target[index] {
            return true;
        }
        if hash[index] > target[index] {
            return false;
        }
    }
    true
}

pub(crate) fn double_sha256(data: &[u8], out: &mut [u8; 32]) {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let first = hasher.finalize();

    let mut hasher2 = Sha256::new();
    hasher2.update(first);
    out.copy_from_slice(&hasher2.finalize());
}
