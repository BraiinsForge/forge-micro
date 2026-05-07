//! 80-byte Bitcoin block-header builder.
//!
//! All inputs come from a `WorkAssignment` already in canonical
//! byte order — the V1 prev-hash word swap and any other protocol
//! quirks landed at the adapter boundary, so this is a memcpy
//! sequence with no per-protocol branches.

use stratum_client::WorkAssignment;

/// Block-header byte length.
pub const HEADER_BYTES: usize = 80;

/// Materialise the 80-byte block header for `work` with `nonce`
/// stamped at byte offset 76.
#[must_use]
pub fn build_header(work: &WorkAssignment, nonce: u32) -> [u8; HEADER_BYTES] {
    let mut header = [0u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&work.version.to_le_bytes());
    header[4..36].copy_from_slice(&work.prev_hash);
    header[36..68].copy_from_slice(&work.merkle_root);
    header[68..72].copy_from_slice(&work.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&work.nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}
