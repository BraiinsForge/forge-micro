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

//! `WorkRing<N>` — outstanding ASIC work.
//!
//! Each slot pairs an `assignment_id`, stamped into the `WorkAssignment`
//! published to the mining actor, with the per-share context the host
//! needs to encode a wire submit: `extranonce_2`, `ntime`,
//! `version_bits`, and the originating `JobId`.
//!
//! When a `Solution` returns from the mining actor, the host looks up the
//! matching `assignment_id`. Eviction is FIFO — older outstanding work
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

    /// Insert a new `OutstandingWork`, evicting the older slot at
    /// this index; a `Solution` that comes back for it later gets
    /// dropped by the lookup miss.
    pub fn put(&mut self, work: OutstandingWork) {
        self.slots[self.write_index] = Some(work);
        self.write_index = (self.write_index + 1) % N;
    }

    /// Return a reference to the slot whose `assignment_id` matches.
    /// `None` if the ring rotated past it.
    #[must_use]
    pub fn lookup(&self, assignment_id: u32) -> Option<&OutstandingWork> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref())
            .find(|work| work.assignment_id == assignment_id)
    }
}

impl<const N: usize> Default for WorkRing<N> {
    fn default() -> Self {
        Self::new()
    }
}
