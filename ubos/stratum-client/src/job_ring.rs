//! `JobRing<N>` — pool-side stratum jobs, depth-2 by default.
//!
//! Each slot holds one job snapshot copied out of a borrowed
//! `MiningJob<'_>` during `visit_new_job`. `clean_jobs=true`
//! invalidates the previous slot so stale shares get filtered
//! before submit hits the wire.

use mining_protocol::JobId;

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
    /// carries `ntime_base + ntime_offset`, with the offset
    /// incremented by the work generator every roll cycle.
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

    /// Reserve and return a mutable handle to the next slot. The
    /// caller fills it from a borrowed `MiningJob<'_>` and sets
    /// `valid = true`. Older slot at this index is dropped.
    pub fn next_slot(&mut self) -> &mut JobSlot {
        let index = self.write_index;
        self.write_index = (self.write_index + 1) % N;
        &mut self.slots[index]
    }

    /// Mark every slot except the most recently written as
    /// `valid = false`. Called when `clean_jobs=true` arrives.
    pub fn invalidate_previous(&mut self) {
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
    /// before the first `next_slot()` populate.
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
