//! `WorkRing<N>` — outstanding ASIC work.
//!
//! Each slot pairs a `work_id` (stamped into the `WorkAssignment`
//! published to the ASIC) with the per-share context the host needs
//! to encode a wire submit (`extranonce_2`, `ntime`,
//! `version_bits`, plus the originating `JobId`).
//!
//! When a `Solution` returns from the ASIC, the host looks up the
//! matching `work_id`. Eviction is FIFO — older outstanding work
//! drops off the ring once the depth is exceeded.

use crate::work::OutstandingWork;

/// Default ring depth. Eight slots covers the "ASIC has work in
/// flight, host hasn't drained the solution channel yet" window
/// without needing dynamic allocation.
pub const WORK_RING_DEPTH: usize = 8;

/// Depth-N FIFO of `OutstandingWork`.
pub struct WorkRing<const N: usize> {
    slots: [Option<OutstandingWork>; N],
    write_index: usize,
}

impl<const N: usize> WorkRing<N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            write_index: 0,
        }
    }

    /// Insert a new `OutstandingWork`. Older slot at this index is
    /// evicted (`Solution`s that come back for it later get dropped
    /// by the lookup miss).
    pub fn put(&mut self, work: OutstandingWork) {
        self.slots[self.write_index] = Some(work);
        self.write_index = (self.write_index + 1) % N;
    }

    /// Return a reference to the slot whose `work_id` matches.
    /// `None` if the ring rotated past it.
    #[must_use]
    pub fn lookup(&self, work_id: u32) -> Option<&OutstandingWork> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref())
            .find(|work| work.work_id == work_id)
    }
}

impl<const N: usize> Default for WorkRing<N> {
    fn default() -> Self {
        Self::new()
    }
}
