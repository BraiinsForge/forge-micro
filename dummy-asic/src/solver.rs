//! Nonce search on `APP_CPU`.
//!
//! Hosts the SHA-256d helper plus the embassy task that sweeps nonces for
//! the current Work item.  The task consumes `CURRENT_WORK` and publishes
//! to `SOLUTIONS` both defined in `main.rs`.
//!
//! SHA-256d uses the ESP32-S3 SHA accelerator.  Software SHA on Xtensa
//! is far slower than the UART can drain, so the hardware peripheral
//! is the actual bound this peer is supposed to demonstrate.

use esp_hal::peripherals::SHA;
use esp_hal::sha::{Sha, Sha256};
use nb::block;

use crate::protocol::{Solution, Work};
use crate::{CURRENT_WORK, SOLUTIONS};

/// Mask applied to the nonce to trigger a cooperative yield. Power of
/// two so the test compiles to a single `and`. 4096 keeps worst-case
/// new-Work latency well under a millisecond at the hardware's hash
/// rate while amortising the yield/`try_changed` overhead.
const SOLVER_YIELD_MASK: u32 = 4096 - 1;

/// Compute SHA-256 of `data` using the SHA peripheral.
fn sha256(sha: &mut Sha<'static>, data: &[u8]) -> [u8; 32] {
    let mut output = [0u8; 32];
    let mut hasher = sha.start::<Sha256>();
    let mut remaining = data;
    while !remaining.is_empty() {
        // The esp-hal SHA driver returns `nb::Result<_, Infallible>`,
        // so `block!` only ever yields `Ok` once the peripheral is
        // ready. The expect is unreachable in practice.
        remaining = block!(hasher.update(remaining)).expect("BUG: SHA update is Infallible");
    }
    // Same Infallible invariant as the update loop above.
    block!(hasher.finish(&mut output)).expect("BUG: SHA finish is Infallible");
    output
}

/// Compute SHA-256d (double-SHA-256) of `data` using the SHA peripheral.
fn hw_double_sha256(sha: &mut Sha<'static>, data: &[u8]) -> [u8; 32] {
    let first = sha256(sha, data);
    sha256(sha, &first)
}

/// Sweep nonces on the `APP_CPU`.  Yields to the embassy executor each
/// time the nonce hits a `SOLVER_YIELD_MASK` boundary to keep other
/// tasks responsive.  Restarts the sweep whenever a new Work item
/// arrives on `CURRENT_WORK`.
#[embassy_executor::task]
pub(crate) async fn solver_task(sha_peripheral: SHA<'static>) -> ! {
    let mut sha = Sha::new(sha_peripheral);
    // Solver is the sole receiver; reader_task is the only publisher.
    let mut receiver = CURRENT_WORK
        .receiver()
        .expect("BUG: Watch<_, _, 1> already has a receiver");

    // Block until the first Work item is available.
    let mut work: Work = receiver.changed().await;
    let mut nonce: u32 = 0;

    loop {
        work.header[76..80].copy_from_slice(&nonce.to_le_bytes());
        let result_hash = hw_double_sha256(&mut sha, &work.header);

        // SHA-256d result is little-endian; trailing bytes are the BE
        // leading zeros.  Check that the tail is all zero.
        let tail = &result_hash[32 - work.target_leading_zero_bytes as usize..];
        if tail.iter().all(|&byte| byte == 0) {
            // try_send drops the new solution on a full channel rather
            // than blocking the solver.  Continuous mining at a low
            // target produces shares faster than the UART can drain;
            // dropping is the right shape for a test harness, the
            // host's hashrate estimate is naturally biased toward
            // delivered shares.
            let _ = SOLUTIONS.try_send(Solution {
                nonce,
                hash: result_hash,
            });
            // Continue the sweep instead of waiting for a new Work
            // request.  The host treats the dummy-asic as a streaming
            // miner: one Work kicks it, shares pour out until a new
            // Work arrives.
        }

        // Wrapping add is intentional. The dummy ASIC contract assumes the
        // host only sends satisfiable targets, so 2^32 is never reached.
        nonce = nonce.wrapping_add(1);

        if nonce & SOLVER_YIELD_MASK == 0 {
            // Check whether a new Work item arrived while we were mining.
            if let Some(new_work) = receiver.try_changed() {
                work = new_work;
                nonce = 0;
            }
            embassy_futures::yield_now().await;
        }
    }
}
