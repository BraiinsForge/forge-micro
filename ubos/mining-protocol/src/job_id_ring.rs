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

//! Fixed-depth ring mapping a generic [`JobId`] to a protocol's wire
//! job id.
//!
//! Shared by `stratum-v1`, whose wire id is a hex string, and `stratum-v2`,
//! whose wire id is a `u32`. `allocate` assigns the next `JobId`, stores
//! the wire id at the current write index, and advances the index
//! modulo the ring depth; `lookup` finds the wire id for a `JobId`
//! that is still in the ring, or reports that it has rotated out.

use crate::JobId;

#[cfg(test)]
#[path = "tests/job_id_ring.rs"]
mod tests;

/// Depth-`N` ring mapping a generic `JobId` to a protocol's wire job
/// id of type `W`.
#[derive(Debug, PartialEq, Eq)]
pub struct JobIdRing<W, const N: usize> {
    slots: [Option<(JobId, W)>; N],
    write_index: usize,
    next_id: u32,
}

impl<W, const N: usize> JobIdRing<W, N> {
    #[must_use]
    pub const fn new() -> Self {
        const { assert!(N > 0, "the ring needs at least one slot") };
        Self {
            slots: [const { None }; N],
            write_index: 0,
            next_id: 0,
        }
    }

    /// Allocate the next `JobId`, store `wire` at the current write
    /// index, and advance the index modulo `N`.
    pub fn allocate(&mut self, wire: W) -> JobId {
        let generic = JobId(self.next_id);
        // `wrapping_add` so a session that outlives 2^32 allocations
        // keeps assigning ids instead of panicking.
        self.next_id = self.next_id.wrapping_add(1);
        let index = self.write_index;
        self.slots[index] = Some((generic, wire));
        self.write_index = (index + 1) % N;
        generic
    }

    /// Find the wire id for `generic`.
    /// Returns `None` if `generic` has rotated out of the ring.
    pub fn lookup(&self, generic: JobId) -> Option<&W> {
        self.slots.iter().find_map(|slot| match slot {
            Some((slot_generic, wire)) if *slot_generic == generic => Some(wire),
            _ => None,
        })
    }
}

impl<W, const N: usize> Default for JobIdRing<W, N> {
    fn default() -> Self {
        Self::new()
    }
}
