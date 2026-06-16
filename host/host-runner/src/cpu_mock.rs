//! CPU-side mock of an ASIC, talking to `stratum-client` through
//! `AsicSession`.
//!
//! Polls the work cell, hashes a chunk of nonces, pushes solutions
//! back. Used as the host-runner fallback when no BM1362 hashboard is
//! available.

use embassy_futures::yield_now;
use stratum_client::{AsicSession, Solution};

use hash::{hash_meets_target, sha256d};
use header::build_header;

#[cfg(test)]
#[path = "tests/cpu_mock.rs"]
mod tests;

mod hash {
    //! SHA-256d primitive and hash-vs-target comparison.

    use sha2::{Digest, Sha256};

    /// Compute `sha256(sha256(input))`.
    #[must_use]
    pub fn sha256d(input: &[u8]) -> [u8; 32] {
        let first = Sha256::digest(input);
        Sha256::digest(first).into()
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
/// promptly. 256 happens to match what `stratum-client` settled on.
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
        while nonce < chunk_end {
            let header = build_header(&current, nonce);
            let hash = sha256d(&header);
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
