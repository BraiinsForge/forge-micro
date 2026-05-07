//! Borrowed `MiningJob` exposed to the visitor.

use crate::job_id::JobId;
use crate::lazy_merkle::LazyMerkle;

/// A mining job in canonical, ready-to-use form.
///
/// All hash-shaped fields are in block-header byte order: a direct
/// memcpy of `prev_hash` lands at `header[4..36]`, the root computed
/// from `merkle` lands at `header[36..68]`. The V1 adapter does the
/// per-u32 `prev_hash` swap internally; consumers do not see the
/// V1 quirk.
///
/// `target` is the 32-byte little-endian 256-bit integer the solver
/// compares the `sha256d(header)` output against (`target[0]` is the
/// least-significant byte).
///
/// All slices borrow from the adapter's wire/decode buffers and are
/// valid only for the duration of one `accept()` call. Visitors
/// must copy out anything they need to keep.
#[derive(Debug, Clone, Copy)]
pub struct MiningJob<'a> {
    /// Adapter-assigned correlation id; opaque to consumers.
    pub id: JobId,
    /// Previous block hash, block-header byte order.
    pub prev_hash: &'a [u8; 32],
    /// Pool-supplied target, 256-bit little-endian.
    pub target: &'a [u8; 32],
    /// Block version field.
    pub version: u32,
    /// Bits permitted for version-rolling (V1 negotiated mask, V2
    /// `SetupConnection.flags`-derived). `0` means no rolling.
    pub version_rolling_mask: u32,
    /// Compact difficulty target.
    pub nbits: u32,
    /// Pool-supplied `ntime` base; the work generator rolls forward
    /// from here within the consensus-allowed window.
    pub ntime: u32,
    /// Whether this notify invalidates older queued jobs.
    pub clean_jobs: bool,
    /// Bytes of `extranonce_2` the miner is expected to roll. `0`
    /// means no extranonce rolling (V2 standard channels).
    pub extranonce_2_size: u8,
    /// Materials for the merkle root (V1) or the precomputed root
    /// itself (V2 standard).
    pub merkle: LazyMerkle<'a>,
}
