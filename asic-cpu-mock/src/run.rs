//! `run_asic_cpu` — the CPU-side mock that drives the ASIC end of
//! `AsicSession`.
//!
//! Polls the work cell, hashes a chunk of nonces against the
//! current target, and pushes any found `Solution` back into the
//! channel. Yields between chunks so the embassy executor stays
//! responsive on the same core.

use embassy_futures::yield_now;
use stratum_client::{AsicSession, Solution, WorkAssignment};

use crate::hash::{hash_meets_target, sha256d};
use crate::header::build_header;

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

/// Side door for unit tests: hash one nonce and report whether it
/// meets the target. Lets tests cover the inner loop without
/// spinning the asynchronous machinery.
#[doc(hidden)]
#[must_use]
pub fn nonce_meets_target(work: &WorkAssignment, nonce: u32) -> bool {
    let header = build_header(work, nonce);
    let mut hash = [0u8; 32];
    sha256d(&header, &mut hash);
    hash_meets_target(&hash, &work.target)
}
