//! `StratumState` — the visitor that owns both rings and the
//! session-level state that flows from `SessionUpdate` events into
//! the next `WorkAssignment`.
//!
//! Implements `MiningClientVisitor` so the V1 / V2 adapter dispatch
//! lands directly here. The same struct is also the work
//! generator's source of truth for the latest job.

use mining_protocol::{MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult};
use ubos_logging::{info, warn};

use crate::job_ring::{JobRing, JOB_RING_DEPTH};
use crate::work::MAX_EXTRANONCE_2;
use crate::work_ring::{WorkRing, WORK_RING_DEPTH};

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

/// Maximum bytes for the pool-side `extranonce_prefix` (V1
/// `extranonce_1`, V2 `extranonce_prefix`). Enough for any pool
/// today; sized like V1's `MAX_EXTRANONCE_1`.
pub const MAX_EXTRANONCE_PREFIX: usize = 8;

/// Visitor + persistent integration state.
pub struct StratumState {
    pub job_ring: JobRing<JOB_RING_DEPTH>,
    pub work_ring: WorkRing<WORK_RING_DEPTH>,
    /// Latest target the pool advertised. Stamped into every new
    /// `JobSlot` by `visit_new_job`; mid-session `SetTarget`
    /// updates this and the current slot in place.
    pub target: [u8; 32],
    /// Latest version-rolling mask. Same flow as `target`.
    pub version_mask: u32,
    /// Latest extranonce-prefix bytes the pool assigned. The host
    /// keeps this for the work generator's `OutstandingWork`
    /// records, even though `WorkAssignment` itself doesn't carry
    /// them (en2 is folded into `merkle_root` pool-side).
    pub extranonce_prefix_len: usize,
    pub extranonce_prefix: [u8; MAX_EXTRANONCE_PREFIX],
    pub extranonce_2_size: u8,
    /// Counter for `WorkAssignment::work_id` allocation in the work
    /// generator.
    pub next_work_id: u32,
    /// Set whenever a `clean_jobs=true` notify lands so the ASIC
    /// driver / work generator can react to a chain switch.
    pub clean_jobs_pending: bool,
}

impl StratumState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            job_ring: JobRing::new(),
            work_ring: WorkRing::new(),
            target: PERMISSIVE_TARGET,
            version_mask: 0,
            extranonce_prefix_len: 0,
            extranonce_prefix: [0; MAX_EXTRANONCE_PREFIX],
            extranonce_2_size: 0,
            next_work_id: 0,
            clean_jobs_pending: false,
        }
    }

    fn store_extranonce_prefix(&mut self, prefix: &[u8], en2_size: u8) {
        let copy_len = prefix.len().min(self.extranonce_prefix.len());
        self.extranonce_prefix[..copy_len].copy_from_slice(&prefix[..copy_len]);
        self.extranonce_prefix_len = copy_len;
        self.extranonce_2_size = en2_size;
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
            job.id.0,
            job.clean_jobs,
            job.ntime,
            job.nbits,
            job.extranonce_2_size,
        );
        let slot = self.job_ring.next_slot();
        slot.job_id = job.id;
        slot.prev_hash = *job.prev_hash;
        // Snapshot the session-level target rather than the borrowed
        // job.target. `mining.set_difficulty` arrives independently
        // of notifies; the visitor's own target field is the
        // authoritative latest value for the next job.
        slot.target = self.target;
        // Build the merkle root once with extranonce_2 = zeros. The
        // CPU mock and current ASIC drivers don't roll en2; when
        // they do, the per-share recompute happens in the work
        // generator with the materials still in scope (a future
        // refinement extends OwnedJob to keep them). Slice is
        // exact-sized: the adapter rejects sessions whose
        // `extranonce_2_size` exceeds `MAX_EXTRANONCE_2`, so the
        // index here is always in range.
        let mut zeros = [0u8; MAX_EXTRANONCE_2];
        let en2_size = usize::from(job.extranonce_2_size);
        debug_assert!(en2_size <= MAX_EXTRANONCE_2);
        let en2 = &zeros[..en2_size];
        job.merkle.compute(en2, &mut slot.merkle_root);
        // Suppress unused-mut on the array; `compute` touches the
        // `out` buffer so `zeros` itself stays unread.
        let _ = &mut zeros;
        slot.version = job.version;
        slot.version_rolling_mask = self.version_mask | job.version_rolling_mask;
        slot.nbits = job.nbits;
        slot.ntime_base = job.ntime;
        slot.ntime_offset = 0;
        slot.extranonce_2_size = job.extranonce_2_size;
        slot.clean_jobs = job.clean_jobs;
        slot.valid = true;

        if job.clean_jobs {
            self.job_ring.invalidate_previous();
            self.clean_jobs_pending = true;
        }
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        if result.accepted {
            info!("stratum-client: share accepted id={}", result.id.0,);
        } else {
            warn!(
                "stratum-client: share rejected id={} code={:?} message={:?}",
                result.id.0,
                result.error_code,
                result.error_message,
            );
        }
    }

    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        match update {
            SessionUpdate::SetTarget(target) => {
                {
                    let target_hex = mining_protocol::hex::encode_block_hash_lower(target);
                    info!(
                        "stratum-client: set target {}...",
                        core::str::from_utf8(&target_hex[..32]).expect("hex encoder emits ASCII"),
                    );
                }
                self.target = *target;
                if let Some(latest) = self.job_ring.current_mut() {
                    latest.target = *target;
                }
            }
            SessionUpdate::SetExtranonce { prefix, en2_size } => {
                let mut prefix_hex = [0u8; MAX_EXTRANONCE_PREFIX * 2];
                let copy_len = prefix.len().min(MAX_EXTRANONCE_PREFIX);
                let prefix_hex_len = copy_len * 2;
                let prefix_hex_ok = mining_protocol::hex::encode_lower_to_slice(
                    &prefix[..copy_len],
                    &mut prefix_hex[..prefix_hex_len],
                );
                debug_assert!(prefix_hex_ok);
                info!(
                    "stratum-client: set extranonce prefix={} en2_size={}",
                    core::str::from_utf8(&prefix_hex[..prefix_hex_len])
                        .expect("hex encoder emits ASCII"),
                    en2_size,
                );
                self.store_extranonce_prefix(prefix, en2_size);
            }
            SessionUpdate::SetVersionMask(mask) => {
                info!("stratum-client: set version mask {:#010x}", mask);
                self.version_mask = mask;
                if let Some(latest) = self.job_ring.current_mut() {
                    latest.version_rolling_mask = mask;
                }
            }
            SessionUpdate::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                info!(
                    "stratum-client: pool reconnect request host={} port={} wait={}",
                    host,
                    port,
                    wait_seconds,
                );
                // Surfacing a reconnect to the firmware loop comes
                // with the migration in phase 8; for now the TCP
                // layer's normal close-and-retry cycle covers it.
            }
        }
    }
}

/// Initial share target before the pool sends its first
/// `mining.set_difficulty` (V1) or `OpenStandardMiningChannel.Success`
/// (V2). Two leading zero bytes corresponds to pool diff 1/256² —
/// the same default `ubos-backend` used.
const PERMISSIVE_TARGET: [u8; 32] = {
    let mut target = [0xFFu8; 32];
    target[31] = 0x00;
    target[30] = 0x00;
    target
};
