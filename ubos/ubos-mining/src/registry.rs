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

//! The in-flight work registry: which assignment each hardware work ID names.
//!
//! A chip echoes the work ID of the assignment it holds, and a driver that writes
//! the chain only when the header changes leaves every chip echoing one
//! work ID for as long as a pool job lasts. So a work ID names an assignment,
//! not a work tick: a tick that repeats the whole assignment the last
//! work ID went out under reuses that ID, and work IDs advance with the
//! work.

#[cfg(test)]
#[path = "tests/registry.rs"]
mod tests;

use crate::consts::WORK_REGISTRY_SLOTS;
use stratum_client::WorkAssignment;
use ubos_asic::JobVersion;
use ubos_logging::warn;

/// In-flight work for a given `work_id` slot. Holds the assignment so an
/// incoming solution can be verified and mapped back to the originating
/// job. The assignment is captured at dispatch time because the
/// session's `Watch` publishes fresh assignments mid-job; shares are
/// credited against the assignment the chip was actually scanning.
pub(crate) struct WorkSlot {
    pub(crate) assignment: WorkAssignment,
    /// Pool slot whose work cell this dispatch was drawn from. The
    /// eventual solution routes back to this slot even if the
    /// scheduler picks other slots while the chip is hashing, so a
    /// session keeps submitting its in-flight shares.
    pub(crate) session_slot: usize,
    /// The version the job went out under, which a solution's echo is
    /// merged into.
    pub(crate) version: JobVersion,
}

/// The assignments in flight, indexed by hardware work ID. The ID is
/// the slot index: the driver maps it to and from the chip's echo field, so a
/// returned solution finds the assignment it was found on.
pub(crate) struct WorkRegistry {
    slots: [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
    /// Work IDs the driver's echo field can carry, `1..=WORK_REGISTRY_SLOTS`.
    work_slots: u32,
    next_work_id: u32,
    /// Work ID the last dispatch went out under, so a repeat of that
    /// assignment finds it. `None` before the first dispatch and after
    /// a reset.
    last_work_id: Option<u32>,
}

impl WorkRegistry {
    /// Size the work-ID space to what the driver's echo field carries,
    /// within this registry's capacity.
    pub(crate) fn new(driver_work_slots: usize) -> Self {
        assert!(
            driver_work_slots >= 1,
            "BUG: driver reports zero work slots"
        );
        if driver_work_slots > WORK_REGISTRY_SLOTS {
            warn!(
                "Driver offers {} work slots, registry truncates to {}",
                driver_work_slots, WORK_REGISTRY_SLOTS
            );
        }
        let work_slots = u32::try_from(driver_work_slots.min(WORK_REGISTRY_SLOTS))
            .expect("BUG: the registry capacity fits a u32");
        Self {
            slots: [const { None }; WORK_REGISTRY_SLOTS],
            work_slots,
            next_work_id: 0,
            last_work_id: None,
        }
    }

    /// Forget every assignment and start the work IDs over, for a chain
    /// that was reset and holds no work.
    pub(crate) fn reset(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.next_work_id = 0;
        self.last_work_id = None;
    }

    /// Record a dispatch and return the hardware work ID. A repeat of
    /// the assignment the last dispatch carried, from the same pool slot,
    /// keeps that ID; anything else takes the next one and retires its
    /// previous assignment. `version` is what the job built from the
    /// assignment carries, kept so a solution is merged under the same
    /// partition it was dispatched with.
    pub(crate) fn place(
        &mut self,
        assignment: WorkAssignment,
        session_slot: usize,
        version: JobVersion,
    ) -> u32 {
        // The whole assignment, not just its assignment ID: a session mints its
        // assignment IDs from zero, so a reconnect on the same pool slot offers id
        // zero again for a job that is not the one in flight, and the
        // work ID would then name the header the previous session was
        // hashing while the chips took the new one.
        let repeat = self.last_work_id.filter(|&work_id| {
            self.slots[work_id as usize].as_ref().is_some_and(|held| {
                held.session_slot == session_slot && held.assignment == assignment
            })
        });
        if let Some(work_id) = repeat {
            return work_id;
        }
        let work_id = self.next_work_id;
        self.slots[work_id as usize] = Some(WorkSlot {
            assignment,
            session_slot,
            version,
        });
        self.next_work_id = (work_id + 1) % self.work_slots;
        self.last_work_id = Some(work_id);
        work_id
    }

    /// The assignment a hardware work ID names. The ID comes from the
    /// chip's echo field, so a stray reply can name an ID past the registry or
    /// one that holds nothing; both read as absent.
    pub(crate) fn get(&self, work_id: u32) -> Option<&WorkSlot> {
        self.slots.get(work_id as usize).and_then(Option::as_ref)
    }
}
