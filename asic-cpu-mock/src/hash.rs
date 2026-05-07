//! SHA-256d primitive and hash-vs-target comparison.

use sha2::{Digest, Sha256};

/// Compute `sha256(sha256(input))` into `out`.
pub fn sha256d(input: &[u8], out: &mut [u8; 32]) {
    let first = Sha256::digest(input);
    let second = Sha256::digest(first);
    out.copy_from_slice(&second);
}

/// Return `true` when `hash` (256-bit little-endian) is less than
/// or equal to `target`. Exact byte-by-byte compare from MSB down.
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
