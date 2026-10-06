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

//! `StratumState` — the visitor that owns both rings and the
//! session-level state that flows from `SessionUpdate` events into
//! the next `WorkAssignment`.
//!
//! Implements `MiningClientVisitor` so the V1 / V2 adapter dispatch
//! lands directly here. The same struct is also the work
//! generator's source of truth for the latest job.
//!
//! The share counters below are free functions over private statics
//! rather than fields of `StratumState`, and that is what keeps them
//! across a reconnect. A reconnect builds a fresh `StratumState`, so a
//! field would reset the lifetime totals every time a pool dropped.

use heapless::Vec;
use ubos_logging::trace;

use crate::asic_session::PoolHandle;
use crate::run::NTIME_ROLL_MAX_SECS;
use crate::work::{MAX_EXTRANONCE_2, OutstandingWork, WorkAssignment};
use mining_protocol::{MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult};
use ubos_logging::{info, warn};

use crate::run::POOL_INDEX_SPACE;

/// Shares the pool has accepted since boot, summed across every pool
/// slot. The only path across the crate boundary; the per-slot
/// counters in [`crate::metrics::POOL_METRICS`] stay crate-private.
#[must_use]
pub fn shares_accepted() -> u64 {
    crate::metrics::POOL_METRICS
        .iter()
        .map(crate::metrics::PoolMetrics::shares_accepted)
        .sum()
}

/// Shares the pool has rejected since boot, summed across every pool
/// slot.
#[must_use]
pub fn shares_rejected() -> u64 {
    crate::metrics::POOL_METRICS
        .iter()
        .map(crate::metrics::PoolMetrics::shares_rejected)
        .sum()
}

use crate::job_ring::{JOB_RING_DEPTH, JobRing};
use crate::work_ring::{WORK_RING_DEPTH, WorkRing};

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

/// Visitor + persistent integration state.
pub struct StratumState {
    pub job_ring: JobRing<JOB_RING_DEPTH>,
    pub work_ring: WorkRing<WORK_RING_DEPTH>,
    /// Counter for `WorkAssignment::assignment_id` allocation in the work
    /// generator.
    pub next_assignment_id: u32,
    /// This session's pool slot, used to index the per-slot share and
    /// reconnect counters and to prefix every visitor log line so
    /// concurrent sessions are distinguishable.
    pub(crate) slot: usize,
    /// The version-rolling mask the pool granted, zero until the
    /// connection handshake reports one.
    pub version_mask: u32,
}

impl StratumState {
    /// Build the next `WorkAssignment` from the current ring slot,
    /// record an `OutstandingWork` so the eventual `Solution` can be
    /// translated back to a `Share`, and publish into the handle's slot.
    ///
    /// `step` advances the slot's `ntime_offset` by that many seconds,
    /// capped at [`NTIME_ROLL_MAX_SECS`], before publishing. The loop in
    /// `run_stratum_client_inner` passes the roll period on each ticker
    /// tick, or `0` for the one-shot publish that hands a freshly accepted
    /// job to the ASIC when no ticker is running.
    pub(crate) fn publish_work<const N: usize, const CHAINS: usize>(
        &mut self,
        asic: PoolHandle<'_, N, CHAINS>,
        step: u32,
    ) {
        let extranonce_2_size = match self.job_ring.current() {
            Some(slot) if slot.valid => slot.extranonce_2_size,
            _ => return,
        };

        if let Some(slot) = self.job_ring.current_mut() {
            slot.ntime_offset = slot
                .ntime_offset
                .saturating_add(step)
                .min(NTIME_ROLL_MAX_SECS);
        }

        let assignment_id = self.next_assignment_id;
        self.next_assignment_id = self.next_assignment_id.wrapping_add(1);

        // Adapters reject the session with
        // `SessionRejection::ExtranonceTooLarge` when the pool advertises
        // an `extranonce_2_size` above the static `MAX_EXTRANONCE_2`,
        // so this expect() can only fire on a violated invariant.
        let mut extranonce_2: Vec<u8, MAX_EXTRANONCE_2> = Vec::new();
        extranonce_2
            .resize_default(usize::from(extranonce_2_size))
            .expect(
                "BUG: SetExtranonce validation should have rejected en2_size > MAX_EXTRANONCE_2",
            );

        self.work_ring.put(OutstandingWork {
            assignment_id,
            job_id: self
                .job_ring
                .current()
                .expect("BUG: current() returned Some above")
                .job_id,
            extranonce_2,
        });

        let slot = self
            .job_ring
            .current()
            .expect("BUG: current() returned Some above");
        let assignment = WorkAssignment {
            assignment_id,
            prev_hash: slot.prev_hash,
            merkle_root: slot.merkle_root,
            target: slot.target,
            version: slot.version,
            nbits: slot.nbits,
            ntime: slot.ntime_base.wrapping_add(slot.ntime_offset),
            version_mask: self.version_mask,
        };
        trace!(
            "stratum-client: publish assignment_id={} job={} ntime={:#010x}",
            assignment.assignment_id, slot.job_id.0, assignment.ntime,
        );
        asic.publish_work(assignment);
    }

    /// Build state for the given pool slot. `slot` is clamped to the
    /// last valid index if it does not fit [`POOL_INDEX_SPACE`].
    #[must_use]
    pub fn new(slot: usize) -> Self {
        debug_assert!(
            slot < POOL_INDEX_SPACE,
            "slot must fit the pool-index space"
        );
        Self {
            job_ring: JobRing::new(),
            work_ring: WorkRing::new(),
            next_assignment_id: 0,
            slot: slot.min(POOL_INDEX_SPACE - 1),
            version_mask: 0,
        }
    }

    /// Reset the per-connection state for a new session on the same slot.
    /// The assignment counter carries on, so a solution queued by the
    /// previous session can never match work the new one hands out.
    pub fn restart(&mut self) {
        *self = Self {
            next_assignment_id: self.next_assignment_id,
            ..Self::new(self.slot)
        };
    }
}

impl MiningClientVisitor for StratumState {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        info!(
            "stratum-client: pool {} new job id={} clean={} ntime={:#010x} \
             nbits={:#010x} en2_size={}",
            self.slot, job.id.0, job.clean_jobs, job.ntime, job.nbits, job.extranonce_2_size,
        );
        // Adapters reject oversized extranonce upstream, but the trait
        // surface still admits a u8 (0..=255). The ring refuses a job
        // it cannot snapshot; drop it here so a misbehaving adapter
        // cannot corrupt the work loop.
        if let Err(e) = self.job_ring.push(&job) {
            warn!(
                "stratum-client: pool {} new job dropped, {:?}",
                self.slot, e
            );
        }
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        match result {
            SubmitResult::Accepted { count, work } => {
                info!(
                    "stratum-client: pool {} {} share(s) accepted, +{} work",
                    self.slot, count, work,
                );
                if let Some(metrics) = crate::metrics::for_slot(self.slot) {
                    metrics.record_shares_accepted(count);
                }
            }
            SubmitResult::Rejected {
                error_code,
                error_message,
            } => {
                warn!(
                    "stratum-client: pool {} share rejected code={:?} message={:?}",
                    self.slot, error_code, error_message,
                );
                if let Some(metrics) = crate::metrics::for_slot(self.slot) {
                    metrics.record_share_rejected();
                }
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
                    "stratum-client: pool {} set target {}...",
                    self.slot,
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
                    "stratum-client: pool {} set extranonce prefix={} en2_size={}",
                    self.slot,
                    core::str::from_utf8(&prefix_hex[..prefix_hex_len])
                        .expect("BUG: hex encoder emits ASCII"),
                    en2_size,
                );
            }
            SessionUpdate::SetVersionMask(mask) => {
                // The granted mask is stamped into every later work
                // assignment, so the chip knows which version bits it
                // may roll.
                self.version_mask = mask;
                info!(
                    "stratum-client: pool {} set version mask {:#010x}",
                    self.slot, mask,
                );
            }
            SessionUpdate::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                info!(
                    "stratum-client: pool {} reconnect request host={} port={} wait={}",
                    self.slot, host, port, wait_seconds,
                );
                // Surfacing a reconnect to the firmware loop comes
                // with the migration in phase 8; for now the TCP
                // layer's normal close-and-retry cycle covers it.
            }
        }
    }
}
