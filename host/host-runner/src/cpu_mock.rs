// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! CPU-side mock of an ASIC, driving `AsicSession` in place of a chain.
//!
//! The fallback the runner spawns when no hashboard is wired.

use embassy_futures::yield_now;
use stratum_client::{AsicSession, Solution};

use hash::{hash_meets_target, sha256d};
use header::build_header;

#[cfg(test)]
#[path = "tests/cpu_mock.rs"]
mod tests;

mod hash {
    use sha2::{Digest, Sha256};

    #[must_use]
    pub fn sha256d(input: &[u8]) -> [u8; 32] {
        let first = Sha256::digest(input);
        Sha256::digest(first).into()
    }

    /// Both values are 256-bit little-endian, so the compare walks down
    /// from the last byte and returns on the first difference.
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
    //! A `WorkAssignment` arrives in canonical byte order, with the V1
    //! prev-hash word swap already done at the adapter boundary, so the
    //! header is a memcpy sequence with no per-protocol branch.

    use stratum_client::WorkAssignment;

    pub const HEADER_BYTES: usize = 80;

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

/// Nonces hashed before the loop yields and re-checks for fresh work.
/// Long enough to amortise the poll, short enough that a work change
/// propagates promptly.
pub const HASHES_PER_CHUNK: u32 = 256;

/// # Panics
///
/// Panics when the work `Watch` has no free receiver slot, which only
/// happens if a second consumer was attached to the same `AsicSession`.
pub async fn run_asic_cpu(asic: &'static AsicSession) -> ! {
    let mut receiver = asic
        .work_receiver(0)
        .expect("BUG: AsicSession Watch must have a free receiver slot");

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
                    assignment_id: current.assignment_id,
                    nonce,
                    ntime: current.ntime,
                    version_bits: 0,
                };
                let _ = asic.submit_solution(0, solution);
            }
            nonce = nonce.wrapping_add(1);
            if nonce == 0 {
                // The nonce space is exhausted for this work.
                current = receiver.changed().await;
                break;
            }
        }
        yield_now().await;
    }
}
