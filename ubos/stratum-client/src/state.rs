//! `StratumState` — the visitor that owns both rings and the
//! session-level state that flows from `SessionUpdate` events into
//! the next `WorkAssignment`.
//!
//! Implements `MiningClientVisitor` so the V1 / V2 adapter dispatch
//! lands directly here. The same struct is also the work
//! generator's source of truth for the latest job.

use mining_protocol::{MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult};
use portable_atomic::{AtomicU64, Ordering};
use ubos_logging::{info, warn};

/// Lifetime-monotonic count of shares the pool accepted. Incremented
/// in [`StratumState::visit_submit_result`] on the `Accepted` arm.
pub static SHARES_ACCEPTED: AtomicU64 = AtomicU64::new(0);

/// Lifetime-monotonic count of shares the pool rejected. Incremented
/// in [`StratumState::visit_submit_result`] on the `Rejected` arm.
pub static SHARES_REJECTED: AtomicU64 = AtomicU64::new(0);

use crate::job_ring::{JobRing, JOB_RING_DEPTH};
use crate::work::MAX_EXTRANONCE_2;
use crate::work_ring::{WorkRing, WORK_RING_DEPTH};

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

/// Visitor + persistent integration state.
pub struct StratumState {
    pub job_ring: JobRing<JOB_RING_DEPTH>,
    pub work_ring: WorkRing<WORK_RING_DEPTH>,
    /// Counter for `WorkAssignment::work_id` allocation in the work
    /// generator.
    pub next_work_id: u32,
}

impl StratumState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            job_ring: JobRing::new(),
            work_ring: WorkRing::new(),
            next_work_id: 0,
        }
    }
}

impl Default for StratumState {
    fn default() -> Self {
        Self::new()
    }
}

impl MiningClientVisitor for StratumState {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        info!(
            "stratum-client: new job id={} clean={} ntime={:#010x} \
             nbits={:#010x} en2_size={}",
            job.id.0, job.clean_jobs, job.ntime, job.nbits, job.extranonce_2_size,
        );
        let en2_size = usize::from(job.extranonce_2_size);
        // The two adapters reject oversized extranonce upstream, but
        // the trait surface still admits a u8 (0..=255). Drop here so
        // a misbehaving adapter cannot panic the work loop with an
        // out-of-bounds slice in release.
        if en2_size > MAX_EXTRANONCE_2 {
            warn!(
                "stratum-client: new job dropped, en2_size={} > MAX_EXTRANONCE_2={}",
                en2_size, MAX_EXTRANONCE_2,
            );
            return;
        }
        let slot = self.job_ring.next_slot();
        slot.job_id = job.id;
        slot.prev_hash = *job.prev_hash;
        slot.target = *job.target;
        // Build the merkle root once with extranonce_2 = zeros. The
        // CPU mock and current ASIC drivers don't roll en2; when
        // they do, the per-share recompute happens in the work
        // generator with the materials still in scope (a future
        // refinement extends OwnedJob to keep them).
        let zeros = [0u8; MAX_EXTRANONCE_2];
        let en2 = &zeros[..en2_size];
        job.merkle.compute(en2, &mut slot.merkle_root);
        slot.version = job.version;
        slot.version_rolling_mask = job.version_rolling_mask;
        slot.nbits = job.nbits;
        slot.ntime_base = job.ntime;
        slot.ntime_offset = 0;
        slot.extranonce_2_size = job.extranonce_2_size;
        slot.clean_jobs = job.clean_jobs;
        slot.valid = true;

        if job.clean_jobs {
            self.job_ring.invalidate_previous();
        }
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        match result {
            SubmitResult::Accepted { count, work } => {
                info!(
                    "stratum-client: {} share(s) accepted, +{} work",
                    count, work,
                );
                SHARES_ACCEPTED.fetch_add(u64::from(count), Ordering::Relaxed);
            }
            SubmitResult::Rejected {
                error_code,
                error_message,
            } => {
                warn!(
                    "stratum-client: share rejected code={:?} message={:?}",
                    error_code, error_message,
                );
                SHARES_REJECTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        match update {
            SessionUpdate::SetTarget(target) => {
                // No state mutation: the next `MiningJob` will carry
                // the updated target stamped by the adapter. This arm
                // is operator visibility on the wire event only.
                let target_hex = mining_protocol::hex::encode_block_hash_lower(target);
                info!(
                    "stratum-client: set target {}...",
                    core::str::from_utf8(&target_hex[..32]).expect("BUG: hex encoder emits ASCII"),
                );
            }
            SessionUpdate::SetExtranonce { prefix, en2_size } => {
                // No state mutation: each `MiningJob` already carries its
                // own `extranonce_2_size` and its `merkle_root` is built
                // from the en1 that was in force at the time the job
                // arrived, so the visitor doesn't need a session-wide
                // copy. This arm is operator visibility on the wire
                // event only.
                //
                // Hex buffer sized for SV2's `Bytes0_32` max; V1 en1
                // (adapter-capped at 8 bytes) fits trivially.
                const MAX_PREFIX_LOG_BYTES: usize = 32;
                let mut prefix_hex = [0u8; MAX_PREFIX_LOG_BYTES * 2];
                let copy_len = prefix.len().min(MAX_PREFIX_LOG_BYTES);
                let prefix_hex_len = copy_len * 2;
                let prefix_hex_ok = mining_protocol::hex::encode_lower_to_slice(
                    &prefix[..copy_len],
                    &mut prefix_hex[..prefix_hex_len],
                );
                debug_assert!(prefix_hex_ok);
                info!(
                    "stratum-client: set extranonce prefix={} en2_size={}",
                    core::str::from_utf8(&prefix_hex[..prefix_hex_len])
                        .expect("BUG: hex encoder emits ASCII"),
                    en2_size,
                );
            }
            SessionUpdate::SetVersionMask(mask) => {
                // No state mutation: same reasoning as `SetTarget`.
                info!("stratum-client: set version mask {:#010x}", mask);
            }
            SessionUpdate::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                info!(
                    "stratum-client: pool reconnect request host={} port={} wait={}",
                    host, port, wait_seconds,
                );
                // Surfacing a reconnect to the firmware loop comes
                // with the migration in phase 8; for now the TCP
                // layer's normal close-and-retry cycle covers it.
            }
        }
    }
}
