//! CPU-side mock of an ASIC, talking to `stratum-client` through
//! `AsicSession`.
//!
//! Polls the work cell, hashes a chunk of nonces, pushes solutions
//! back. Used as the host-runner fallback when no BM1362 hashboard is
//! available.

use embassy_futures::yield_now;
#[cfg(test)]
use stratum_client::WorkAssignment;
use stratum_client::{AsicSession, Solution};

use hash::{hash_meets_target, sha256d};
use header::build_header;

mod hash {
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
}

mod header {
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
}

/// Number of nonces hashed before the loop yields and re-checks
/// for a fresh `WorkAssignment`. Sized so a Cortex-M0+ takes
/// roughly tens of milliseconds per chunk — enough to amortise the
/// poll overhead, short enough that work changes propagate
/// promptly. 256 happens to match what `ubos-backend` settled on.
pub const HASHES_PER_CHUNK: u32 = 256;

/// Run the CPU-mock ASIC forever.
///
/// The function never returns: it awaits initial work, scans
/// nonces in chunks, restarts whenever the `Watch` reports a new
/// `WorkAssignment`, and submits solutions through the
/// `AsicSession::solutions` channel.
///
/// # Panics
///
/// Panics if the `AsicSession`'s work `Watch` has no free
/// receiver slot. With the default `AsicSession::new()` and a
/// single CPU mock that won't happen; misuse from the firmware
/// (e.g. multiple consumers on the same `AsicSession`) is the only
/// way to trigger it.
pub async fn run_asic_cpu(asic: &'static AsicSession) -> ! {
    let mut receiver = asic
        .work_receiver()
        .expect("BUG: AsicSession Watch must have a free receiver slot");

    // Block until the host publishes the first WorkAssignment.
    let mut current = receiver.changed().await;
    let mut nonce: u32 = 0;

    loop {
        if let Some(fresh) = receiver.try_changed() {
            current = fresh;
            nonce = 0;
        }

        let chunk_end = nonce.saturating_add(HASHES_PER_CHUNK);
        let mut hash = [0u8; 32];
        while nonce < chunk_end {
            let header = build_header(&current, nonce);
            sha256d(&header, &mut hash);
            if hash_meets_target(&hash, &current.target) {
                let solution = Solution {
                    work_id: current.work_id,
                    nonce,
                    ntime: current.ntime,
                    version_bits: 0,
                };
                let _ = asic.submit_solution(solution);
            }
            nonce = nonce.wrapping_add(1);
            if nonce == 0 {
                // Wrapped — done with this work, wait for new.
                current = receiver.changed().await;
                break;
            }
        }
        yield_now().await;
    }
}

/// Hash one nonce and report whether it meets the target.
///
/// Lets tests cover the inner loop without spinning the asynchronous
/// machinery.
#[cfg(test)]
#[must_use]
fn nonce_meets_target(work: &WorkAssignment, nonce: u32) -> bool {
    let header = build_header(work, nonce);
    let mut hash = [0u8; 32];
    sha256d(&header, &mut hash);
    hash_meets_target(&hash, &work.target)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the CPU-mock ASIC primitives.

    use super::*;

    fn fixture(target: [u8; 32]) -> WorkAssignment {
        WorkAssignment {
            work_id: 1,
            prev_hash: [0; 32],
            merkle_root: [0; 32],
            target,
            version: 0x2000_0000,
            version_mask: 0,
            nbits: 0x1d00_ffff,
            ntime: 0x6044_9522,
        }
    }

    #[test]
    fn hash_meets_max_target_for_any_nonce() {
        // `target = 0xFF..FF` is the most permissive — every hash wins.
        let work = fixture([0xFFu8; 32]);
        assert!(nonce_meets_target(&work, 0));
        assert!(nonce_meets_target(&work, 0x1234_5678));
    }

    #[test]
    fn hash_misses_zero_target() {
        // Zero target is unreachable.
        let work = fixture([0u8; 32]);
        assert!(!nonce_meets_target(&work, 0));
        assert!(!nonce_meets_target(&work, 1));
    }

    #[test]
    fn build_header_layout_is_canonical() {
        let work = WorkAssignment {
            work_id: 0,
            prev_hash: [0xAA; 32],
            merkle_root: [0xBB; 32],
            target: [0; 32],
            version: 0x1234_5678,
            version_mask: 0,
            nbits: 0x9ABC_DEF0,
            ntime: 0x6112_3456,
        };
        let header = build_header(&work, 0xCAFE_BABE);

        // version little-endian
        assert_eq!(&header[0..4], &[0x78, 0x56, 0x34, 0x12]);
        // prev_hash bytes verbatim
        assert_eq!(&header[4..36], &[0xAA; 32]);
        // merkle_root bytes verbatim
        assert_eq!(&header[36..68], &[0xBB; 32]);
        // ntime little-endian
        assert_eq!(&header[68..72], &[0x56, 0x34, 0x12, 0x61]);
        // nbits little-endian
        assert_eq!(&header[72..76], &[0xF0, 0xDE, 0xBC, 0x9A]);
        // nonce little-endian
        assert_eq!(&header[76..80], &[0xBE, 0xBA, 0xFE, 0xCA]);
    }
}
