//! `StratumState` — the visitor that owns both rings and the
//! session-level state that flows from `SessionUpdate` events into
//! the next `WorkAssignment`.
//!
//! Implements `MiningClientVisitor` so the V1 / V2 adapter dispatch
//! lands directly here. The same struct is also the work
//! generator's source of truth for the latest job.

use mining_protocol::{
    MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult,
};

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
        // refinement extends OwnedJob to keep them).
        let mut zeros = [0u8; MAX_EXTRANONCE_2];
        let en2 = &zeros[..job.extranonce_2_size as usize];
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

    fn visit_submit_result(&mut self, _result: SubmitResult<'_>) {
        // Today the integration layer only logs submit outcomes;
        // structured handling (accepted-share counters, error
        // surfaces to the firmware metrics page) lands with the
        // firmware migration in phase 8.
    }

    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        match update {
            SessionUpdate::SetTarget(target) => {
                self.target = *target;
                if let Some(latest) = self.job_ring.current_mut() {
                    latest.target = *target;
                }
            }
            SessionUpdate::SetExtranonce { prefix, en2_size } => {
                self.store_extranonce_prefix(prefix, en2_size);
            }
            SessionUpdate::SetVersionMask(mask) => {
                self.version_mask = mask;
                if let Some(latest) = self.job_ring.current_mut() {
                    latest.version_rolling_mask = mask;
                }
            }
            SessionUpdate::Reconnect { .. } => {
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
