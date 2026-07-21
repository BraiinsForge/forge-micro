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

//! `JobRing<N>` — pool-side stratum jobs, depth-2 by default.
//!
//! Each slot holds one job snapshot copied out of a borrowed
//! `MiningJob<'_>` during `visit_new_job`. `clean_jobs=true`
//! invalidates the previous slot so stale shares get filtered
//! before submit hits the wire.

use mining_protocol::{JobId, MAX_EXTRANONCE_2, MiningJob};

/// Why a borrowed [`MiningJob`] could not be snapshotted into a
/// [`JobSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum JobSlotError {
    /// The job's `extranonce_2_size` exceeds the static per-share
    /// buffer ([`MAX_EXTRANONCE_2`]), so the merkle root cannot be
    /// built. Adapters reject oversized extranonce upstream, so this
    /// only guards against a misbehaving adapter.
    ExtranonceTooLarge { size: usize },
}

/// Default ring depth. Two slots covers the "current job + the
/// one that just rotated" window most pools accept stale shares
/// against. Promote to a `const` parameter when an ASIC driver
/// shows we need deeper history.
pub const JOB_RING_DEPTH: usize = 2;

/// One pool-side job snapshot, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSlot {
    pub job_id: JobId,
    pub valid: bool,
    pub prev_hash: [u8; 32],
    pub target: [u8; 32],
    pub merkle_root: [u8; 32],
    pub version: u32,
    pub nbits: u32,
    /// Pool-supplied `ntime` base. The published `WorkAssignment`
    /// carries `ntime_base + ntime_offset`, with the offset advanced
    /// directly by the work generator's per-publish step every publish.
    pub ntime_base: u32,
    /// Seconds rolled forward from `ntime_base`. Reset to 0 when
    /// the slot is freshly populated.
    pub ntime_offset: u32,
    pub extranonce_2_size: u8,
    pub clean_jobs: bool,
}

impl JobSlot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            job_id: JobId(0),
            valid: false,
            prev_hash: [0; 32],
            target: [0; 32],
            merkle_root: [0; 32],
            version: 0,
            nbits: 0,
            ntime_base: 0,
            ntime_offset: 0,
            extranonce_2_size: 0,
            clean_jobs: false,
        }
    }

    /// Snapshot a borrowed job into an owned, valid slot with
    /// `ntime_offset` reset to 0.
    ///
    /// Build the merkle root once with `extranonce_2 = zeros`. The
    /// CPU mock and current ASIC drivers don't roll en2; when they
    /// do, the per-share recompute happens in the work generator with
    /// the materials still in scope (a future refinement extends
    /// `OwnedJob` to keep them).
    ///
    /// # Errors
    ///
    /// Returns [`JobSlotError::ExtranonceTooLarge`] when
    /// `extranonce_2_size` exceeds [`MAX_EXTRANONCE_2`], since the
    /// merkle root cannot be computed against a too-small buffer.
    pub fn from_job(job: &MiningJob<'_>) -> Result<Self, JobSlotError> {
        let en2_size = usize::from(job.extranonce_2_size);
        if en2_size > MAX_EXTRANONCE_2 {
            return Err(JobSlotError::ExtranonceTooLarge { size: en2_size });
        }
        let zeros = [0u8; MAX_EXTRANONCE_2];
        Ok(Self {
            job_id: job.id,
            valid: true,
            prev_hash: *job.prev_hash,
            target: *job.target,
            merkle_root: job.merkle.compute(&zeros[..en2_size]),
            version: job.version,
            nbits: job.nbits,
            ntime_base: job.ntime,
            ntime_offset: 0,
            extranonce_2_size: job.extranonce_2_size,
            clean_jobs: job.clean_jobs,
        })
    }
}

/// Depth-N rotating ring of `JobSlot`s.
pub struct JobRing<const N: usize> {
    slots: [JobSlot; N],
    write_index: usize,
}

impl<const N: usize> JobRing<N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| JobSlot::empty()),
            write_index: 0,
        }
    }

    /// Snapshot `job` into the next slot and rotate the ring,
    /// dropping the oldest slot at that index. When `job.clean_jobs`
    /// is set, older slots are invalidated so stale shares get
    /// filtered before submit hits the wire.
    ///
    /// The ring is left untouched on error, so a rejected job causes
    /// no partial rotation.
    ///
    /// # Errors
    ///
    /// Propagates [`JobSlot::from_job`]'s error when `job` cannot be
    /// snapshotted.
    pub fn push(&mut self, job: &MiningJob<'_>) -> Result<(), JobSlotError> {
        let slot = JobSlot::from_job(job)?;
        let index = self.write_index;
        self.slots[index] = slot;
        self.write_index = (self.write_index + 1) % N;
        if job.clean_jobs {
            self.invalidate_previous();
        }
        Ok(())
    }

    /// Mark every slot except the most recently written as
    /// `valid = false`. Called from [`push`](Self::push) when
    /// `clean_jobs=true` arrives.
    fn invalidate_previous(&mut self) {
        let latest = (self.write_index + N - 1) % N;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if index != latest {
                slot.valid = false;
            }
        }
    }

    /// True when `job_id` matches a slot whose `valid` bit is set.
    #[must_use]
    pub fn is_valid(&self, job_id: JobId) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.valid && slot.job_id == job_id)
    }

    /// Borrow the most recently written valid slot. Returns `None`
    /// before the first `push()` populate.
    #[must_use]
    pub fn current(&self) -> Option<&JobSlot> {
        let latest = (self.write_index + N - 1) % N;
        let slot = &self.slots[latest];
        if slot.valid { Some(slot) } else { None }
    }

    /// Mutable view of the most recently written valid slot.
    /// Used by the ntime-offset roller so each published work bumps
    /// the in-flight job's `ntime` within the consensus-allowed window.
    pub fn current_mut(&mut self) -> Option<&mut JobSlot> {
        let latest = (self.write_index + N - 1) % N;
        let slot = &mut self.slots[latest];
        if slot.valid { Some(slot) } else { None }
    }
}

impl<const N: usize> Default for JobRing<N> {
    fn default() -> Self {
        Self::new()
    }
}
