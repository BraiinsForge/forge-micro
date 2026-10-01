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

//! Persistent V2 session state — what the adapter remembers across
//! drive/accept cycles.
//!
//! Lives on `StratumV2Client` so it survives the borrow scope of a
//! single `accept()` call. Wire-borrowed values (target, extranonce
//! prefix, prev hash, merkle root) get copied here as owned storage
//! before any visitor invocation, then reborrowed as `&[u8; 32]` /
//! `&[u8]` for the `MiningJob` and `SessionUpdate` slices the
//! visitor receives.

use ii_sv2::messages::{NewMiningJob, OpenStandardMiningChannelSuccess, SetNewPrevHash, SetTarget};
use mining_protocol::{JobId, JobIdRing};
use mining_protocol::{LazyMerkle, MiningClientVisitor, MiningJob, SessionUpdate};
use ubos_logging::{info, warn};

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

/// Maximum extranonce prefix size returned by an
/// `OpenStandardMiningChannelSuccess`. The wire type `Bytes0_32`
/// caps the value at 32 bytes.
pub const MAX_EXTRANONCE_PREFIX: usize = 32;

/// Depth of the `JobId`-to-wire-id ring. Matches `stratum-client`'s
/// `JobRing` depth so the two rotate in lockstep.
pub const JOBID_MAP_DEPTH: usize = 2;

/// Future-job slot for `NewMiningJob` with `min_ntime: None`. The
/// pool sends future jobs ahead of the matching `SetNewPrevHash`;
/// activation copies these fields into a `MiningJob` once a
/// `SetNewPrevHash` arrives whose `job_id` matches `wire_job_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFutureJob {
    pub wire_job_id: u32,
    pub version: u32,
    pub merkle_root: [u8; 32],
}

/// One row of the `JobId`-to-wire-id translation ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobIdSlot {
    pub generic: JobId,
    pub wire: u32,
    /// Base block `version` the pool published for this job. The
    /// submit path ORs the chip's rolled BIP320 bits into this base
    /// to recover the full `nVersion` the chip actually hashed, since
    /// `SubmitSharesStandard.version` carries the full field, not just
    /// the rolled bits.
    pub version: u32,
}

/// Adapter session state for a V2 standard channel.
#[derive(PartialEq, Eq, Debug)]
pub struct V2SessionState {
    /// `channel_id` from `OpenStandardMiningChannelSuccess`. `None`
    /// until the channel-open response arrives. `submit` errors
    /// with `Adapter` while this is `None`.
    pub(crate) channel_id: Option<u32>,
    /// Previous-block hash from the most recent `SetNewPrevHash`,
    /// in block-header byte order.
    pub(crate) prev_hash: [u8; 32],
    /// Whether `prev_hash` has been populated. The first
    /// `NewMiningJob` cannot be emitted until `SetNewPrevHash` has
    /// supplied this.
    pub(crate) have_prev_hash: bool,
    /// Most-recent share target, 256-bit little-endian. Initialised
    /// from `OpenStandardMiningChannelSuccess.target`; `SetTarget`
    /// overwrites.
    pub(crate) target: [u8; 32],
    /// `extranonce_prefix` from the channel-open success. Up to
    /// `MAX_EXTRANONCE_PREFIX` bytes; valid through
    /// `extranonce_prefix_len`.
    pub(crate) extranonce_prefix: [u8; MAX_EXTRANONCE_PREFIX],
    pub(crate) extranonce_prefix_len: usize,
    /// `nbits` from the most-recent `SetNewPrevHash`.
    pub(crate) nbits: u32,
    /// Pool-supplied `min_ntime` from the most-recent `SetNewPrevHash`,
    /// reserved for the work generator once V2 submit reads it.
    #[allow(dead_code)]
    pub(crate) ntime_base: u32,
    /// Per-channel `sequence_number` for outbound
    /// `SubmitSharesStandard`. The pool echoes the highest seen
    /// value back in `SubmitSharesSuccess.last_sequence_number`.
    pub(crate) next_sequence_number: u32,
    /// Ring mapping `JobId` to the V2 wire u32 `job_id` and the base
    /// `version` the job was built with; see [`JOBID_MAP_DEPTH`] for
    /// why its depth is fixed. Submit looks up the wire id to populate
    /// `SubmitSharesStandard.job_id`.
    job_id_ring: JobIdRing<(u32, u32), JOBID_MAP_DEPTH>,
    /// Pending future `NewMiningJob`, received with `min_ntime: None`,
    /// waiting for a matching `SetNewPrevHash` to activate it.
    pub(crate) pending_future_job: Option<PendingFutureJob>,
}

impl V2SessionState {
    /// `OpenStandardMiningChannelSuccess` carries the operating
    /// `channel_id`, the initial `target`, and the `extranonce_prefix`
    /// the standard channel will use. All three are persisted on
    /// `session`; the visitor then sees `SetTarget` and `SetExtranonce`
    /// updates so the integration layer can stamp them into the next
    /// `MiningJob`.
    pub(crate) fn handle_channel_open_success<V: MiningClientVisitor>(
        &mut self,
        success: &OpenStandardMiningChannelSuccess<'_>,
        visitor: &mut V,
    ) {
        self.channel_id = Some(success.channel_id);
        self.target.copy_from_slice(success.target.to_le_bytes());
        let prefix = success.extranonce_prefix.as_slice();
        // The accept-side validation rejects the session if
        // `prefix.len() > MAX_EXTRANONCE_PREFIX`, so this fits exactly.
        let prefix_len = prefix.len();
        debug_assert!(prefix_len <= MAX_EXTRANONCE_PREFIX);
        self.extranonce_prefix_len = prefix_len;
        self.extranonce_prefix[..prefix_len].copy_from_slice(prefix);
        {
            let target_hex = mining_protocol::hex::encode_block_hash_lower(&self.target);
            let diff = mining_protocol::difficulty::approx_string(&self.target);
            let mut prefix_hex = [0u8; MAX_EXTRANONCE_PREFIX * 2];
            let prefix_hex_len = prefix_len * 2;
            let prefix_hex_ok = mining_protocol::hex::encode_lower_to_slice(
                &self.extranonce_prefix[..prefix_len],
                &mut prefix_hex[..prefix_hex_len],
            );
            debug_assert!(prefix_hex_ok);
            info!(
                "stratum-v2: channel-open-success channel={} prefix={} \
                 target={}... diff={}",
                success.channel_id,
                core::str::from_utf8(&prefix_hex[..prefix_hex_len])
                    .expect("hex encoder emits ASCII"),
                core::str::from_utf8(&target_hex[..32]).expect("hex encoder emits ASCII"),
                diff.as_str(),
            );
        }
        visitor.visit_session_update(SessionUpdate::SetTarget(&self.target));
        visitor.visit_session_update(SessionUpdate::SetExtranonce {
            prefix: &self.extranonce_prefix[..self.extranonce_prefix_len],
            en2_size: 0,
        });
    }

    /// Mid-session `SetTarget` overwrites the channel target.
    pub(crate) fn handle_set_target<V: MiningClientVisitor>(
        &mut self,
        set_target: &SetTarget<'_>,
        visitor: &mut V,
    ) {
        self.target
            .copy_from_slice(set_target.max_target.to_le_bytes());
        {
            let target_hex = mining_protocol::hex::encode_block_hash_lower(&self.target);
            let diff = mining_protocol::difficulty::approx_string(&self.target);
            info!(
                "stratum-v2: set-target channel={} target={}... diff={}",
                set_target.channel_id,
                core::str::from_utf8(&target_hex[..32]).expect("hex encoder emits ASCII"),
                diff.as_str(),
            );
        }
        visitor.visit_session_update(SessionUpdate::SetTarget(&self.target));
    }

    /// `SetNewPrevHash` advances the chain tip. It records the new
    /// `prev_hash`, `nbits`, and `min_ntime` base, then activates any
    /// pending future job whose wire `job_id` matches.
    pub(crate) fn handle_set_new_prev_hash<V: MiningClientVisitor>(
        &mut self,
        snph: &SetNewPrevHash<'_>,
        visitor: &mut V,
    ) {
        self.prev_hash.copy_from_slice(snph.prev_hash.to_le_bytes());
        self.have_prev_hash = true;
        self.nbits = snph.nbits;
        self.ntime_base = snph.min_ntime;
        {
            let prev_hex = mining_protocol::hex::encode_block_hash_lower(&self.prev_hash);
            info!(
                "stratum-v2: set-new-prev-hash channel={} job={} \
                 nbits={:#010x} ntime={:#010x} prev_hash={}",
                snph.channel_id,
                snph.job_id,
                snph.nbits,
                snph.min_ntime,
                core::str::from_utf8(&prev_hex).expect("hex encoder emits ASCII"),
            );
        }

        let Some(pending) = self.pending_future_job.take() else {
            return;
        };
        if pending.wire_job_id != snph.job_id {
            // Future job was for a different job_id — drop it; the
            // pool will resend if it still applies.
            warn!(
                "stratum-v2: pending future job dropped, job_id mismatch \
                 (pending={}, snph={})",
                pending.wire_job_id, snph.job_id,
            );
            return;
        }
        self.emit_mining_job(
            pending.wire_job_id,
            pending.version,
            &pending.merkle_root,
            snph.min_ntime,
            true,
            visitor,
        );
    }

    /// `NewMiningJob` either activates immediately (when `min_ntime` is
    /// `Some`, meaning the job applies to the current `prev_hash`) or
    /// is queued as a pending future job (when `min_ntime` is `None`,
    /// meaning it activates with a later `SetNewPrevHash`).
    pub(crate) fn handle_new_mining_job<V: MiningClientVisitor>(
        &mut self,
        job: &NewMiningJob<'_>,
        visitor: &mut V,
    ) {
        if let Some(ntime) = job.min_ntime {
            if !self.have_prev_hash {
                // Pool sent an active job before any prev hash — drop;
                // the next `SetNewPrevHash` will reset state.
                warn!(
                    "stratum-v2: new-mining-job dropped, no prev_hash yet (job={})",
                    job.job_id,
                );
                return;
            }
            info!(
                "stratum-v2: new-mining-job channel={} job={} \
                 version={:#010x} ntime={:#010x}",
                job.channel_id, job.job_id, job.version, ntime,
            );
            self.emit_mining_job(
                job.job_id,
                job.version,
                job.merkle_root.to_le_bytes(),
                ntime,
                false,
                visitor,
            );
            return;
        }

        let mut merkle_root = [0u8; 32];
        merkle_root.copy_from_slice(job.merkle_root.to_le_bytes());
        info!(
            "stratum-v2: new-mining-job pending channel={} job={} version={:#010x}",
            job.channel_id, job.job_id, job.version,
        );
        self.pending_future_job = Some(PendingFutureJob {
            wire_job_id: job.job_id,
            version: job.version,
            merkle_root,
        });
    }

    /// Allocate a `JobId`, build the canonical `MiningJob`, and hand it
    /// to `visitor`. The `merkle_root` borrow can come from either the
    /// wire buffer (immediate `NewMiningJob`) or the pending-future
    /// slot — either is valid for the duration of the visitor call.
    pub(crate) fn emit_mining_job<V: MiningClientVisitor>(
        &mut self,
        wire_job_id: u32,
        version: u32,
        merkle_root: &[u8; 32],
        ntime: u32,
        clean_jobs: bool,
        visitor: &mut V,
    ) {
        let job_id = self.allocate_job_id(wire_job_id, version);
        let mining_job = MiningJob {
            id: job_id,
            prev_hash: &self.prev_hash,
            target: &self.target,
            version,
            nbits: self.nbits,
            ntime,
            clean_jobs,
            // V2 standard channels do not carry `extranonce_2` —
            // every share rolls only the nonce/ntime/version bits.
            extranonce_2_size: 0,
            merkle: LazyMerkle::Final(merkle_root),
        };
        visitor.visit_new_job(mining_job);
    }

    #[must_use]
    pub const fn new() -> Self {
        Self {
            channel_id: None,
            prev_hash: [0; 32],
            have_prev_hash: false,
            target: [0xFF; 32],
            extranonce_prefix: [0; MAX_EXTRANONCE_PREFIX],
            extranonce_prefix_len: 0,
            nbits: 0,
            ntime_base: 0,
            next_sequence_number: 0,
            job_id_ring: JobIdRing::new(),
            pending_future_job: None,
        }
    }

    /// Allocate the next `JobId` and stash the `(JobId, wire_id,
    /// version)` triple into the ring at the current write index.
    pub fn allocate_job_id(&mut self, wire_id: u32, version: u32) -> JobId {
        self.job_id_ring.allocate((wire_id, version))
    }

    /// Look up the ring slot for a previously emitted `JobId`. The
    /// submit path reads both the wire `job_id` and the base `version`
    /// from it. Returns `None` if the slot has rotated out of the
    /// depth-`JOBID_MAP_DEPTH` ring.
    #[must_use]
    pub fn lookup_job(&self, generic: JobId) -> Option<JobIdSlot> {
        let (wire, version) = *self.job_id_ring.lookup(generic)?;
        Some(JobIdSlot {
            generic,
            wire,
            version,
        })
    }

    /// Reserve and return the next per-channel `sequence_number`.
    pub fn allocate_sequence_number(&mut self) -> u32 {
        let value = self.next_sequence_number;
        self.next_sequence_number = self.next_sequence_number.wrapping_add(1);
        value
    }
}

impl Default for V2SessionState {
    fn default() -> Self {
        Self::new()
    }
}
