//! Protocol-bridging merkle root computation.
//!
//! V1 `mining.notify` carries coinbase parts plus a merkle path; the
//! root has to be hashed up from those inputs and the chosen
//! `extranonce_2`. V2 `NewMiningJob` (standard channels) carries the
//! root directly, with no per-share recomputation.
//!
//! `LazyMerkle::compute` hides the difference behind one method —
//! both adapters yield a `LazyMerkle<'_>` to the visitor; consumers
//! call `compute(en2)` to get the root regardless of protocol.

use sha2::{Digest, Sha256};

use crate::hashing::double_sha256;

#[cfg(test)]
#[path = "tests/lazy_merkle.rs"]
mod tests;

/// Either the materials needed to hash a merkle root from scratch
/// (V1) or a precomputed root borrowed from the wire buffer (V2).
///
/// All slices are bound to the lifetime of the adapter's wire/decode
/// buffers; the value lives only across one `accept()` visitor call.
#[derive(Debug, Clone, Copy)]
pub enum LazyMerkle<'a> {
    /// V1: hash from coinbase + branches.
    Prepared {
        /// V1 `extranonce_1`, decoded to bytes.
        extranonce_prefix: &'a [u8],
        /// V1 `coinbase_1`, hex-decoded.
        coinbase_prefix: &'a [u8],
        /// V1 `coinbase_2`, hex-decoded.
        coinbase_suffix: &'a [u8],
        /// `mining.notify` merkle branches in natural byte order.
        branches: &'a [[u8; 32]],
    },
    /// V2 standard channels: the pool already did the work.
    /// Also used for V1 in the (currently theoretical) zero-size
    /// `extranonce_2` case.
    Final(&'a [u8; 32]),
}

impl LazyMerkle<'_> {
    /// Compute the merkle root for a given `extranonce_2` value.
    ///
    /// `Prepared` builds `coinbase_prefix || extranonce_prefix ||
    /// extranonce_2 || coinbase_suffix`, takes its double SHA-256,
    /// then folds in each branch via `double_sha256(node || branch)`.
    /// The returned root is in canonical block-header byte order — a
    /// direct memcpy into `header[36..68]`.
    ///
    /// `Final` ignores `extranonce_2` (the wire form already absorbed
    /// it pool-side) and returns the precomputed root.
    #[must_use]
    pub fn compute(&self, extranonce_2: &[u8]) -> [u8; 32] {
        match self {
            LazyMerkle::Final(root) => **root,
            LazyMerkle::Prepared {
                extranonce_prefix,
                coinbase_prefix,
                coinbase_suffix,
                branches,
            } => {
                let mut node = hash_coinbase_streamed(
                    coinbase_prefix,
                    extranonce_prefix,
                    extranonce_2,
                    coinbase_suffix,
                );
                let mut concat = [0u8; 64];
                for branch in *branches {
                    concat[..32].copy_from_slice(&node);
                    concat[32..].copy_from_slice(branch);
                    node = double_sha256(&concat);
                }
                node
            }
        }
    }
}

/// `sha256d(coinbase_prefix || extranonce_prefix || extranonce_2 ||
/// coinbase_suffix)` without materialising the concatenation in a
/// single buffer. This avoids a worst-case ~2 KiB scratch for V1's
/// coinbase parts — the inner SHA-256 absorbs the four pieces
/// streamed into the same hasher state.
fn hash_coinbase_streamed(
    coinbase_prefix: &[u8],
    extranonce_prefix: &[u8],
    extranonce_2: &[u8],
    coinbase_suffix: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(coinbase_prefix);
    hasher.update(extranonce_prefix);
    hasher.update(extranonce_2);
    hasher.update(coinbase_suffix);
    let first = hasher.finalize();
    let mut hasher2 = Sha256::new();
    hasher2.update(first);
    hasher2.finalize().into()
}
