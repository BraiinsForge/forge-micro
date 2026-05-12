//! `WorkAssignment` and `Solution` — the host↔ASIC contract.
//!
//! `WorkAssignment` carries everything an ASIC needs to mine on a
//! single header (after en2 has been absorbed pool-side via the
//! merkle root). `Solution` echoes back the `work_id` so the host can
//! reconstruct the full submit context from its `WorkRing`.

use mining_protocol::JobId;
pub use mining_protocol::MAX_EXTRANONCE_2;

/// Work pushed from the host to the ASIC for one round of hashing.
///
/// All hash-shaped fields are in canonical block-header byte order
/// — direct memcpy into the corresponding `[u8; 80]` header slots.
/// `extranonce_2` is intentionally absent: it has already been
/// folded into `merkle_root` before the assignment is published.
#[derive(Clone, Copy)]
pub struct WorkAssignment {
    /// Adapter-stamped correlation token. The ASIC echoes it back
    /// in the `Solution` so the host can recover the matching
    /// per-share state from the `WorkRing`.
    pub work_id: u32,
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub target: [u8; 32],
    pub version: u32,
    pub version_mask: u32,
    pub nbits: u32,
    pub ntime: u32,
}

/// Found nonce returned from the ASIC.
#[derive(Clone, Copy)]
pub struct Solution {
    pub work_id: u32,
    pub nonce: u32,
    pub ntime: u32,
    pub version_bits: u32,
}

/// Per-share context the host needs to construct a `Share` for the
/// pool, recovered from the `WorkRing` on solution lookup. `ntime`
/// and `version_bits` aren't kept here — the ASIC echoes whichever
/// values it actually used directly in the `Solution`.
#[derive(Clone)]
pub struct OutstandingWork {
    pub work_id: u32,
    pub job_id: JobId,
    pub extranonce_2: heapless::Vec<u8, MAX_EXTRANONCE_2>,
}
