//! Protocol-bridging merkle root computation.
//!
//! V1 `mining.notify` carries coinbase parts plus a merkle path; the
//! root has to be hashed up from those inputs and the chosen
//! `extranonce_2`. V2 `NewMiningJob` (standard channels) carries the
//! root directly, with no per-share recomputation.
//!
//! `LazyMerkle::compute` hides the difference behind one method —
//! both adapters yield a `LazyMerkle<'_>` to the visitor; consumers
//! call `compute(en2, &mut root)` regardless of protocol.

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
    /// extranonce_2 || coinbase_suffix`, takes its `sha256d`, then
    /// folds in each branch via `sha256d(node || branch)`. Result
    /// goes into `out` in canonical block-header byte order — direct
    /// memcpy into `header[36..68]`.
    ///
    /// `Final` ignores `extranonce_2` (the wire form already absorbed
    /// it pool-side) and copies the precomputed root.
    pub fn compute(&self, _extranonce_2: &[u8], _out: &mut [u8; 32]) {
        // TODO(BOM-1619): implement once the host harness or unit
        // tests can provide V1 vectors. Phase 2 lands the type; the
        // hashing body is filled in alongside the V1 adapter (phase 5).
        unimplemented!("LazyMerkle::compute lands with the V1 adapter")
    }
}
