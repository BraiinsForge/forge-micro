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

//! The in-flight work registry: which assignment each job key names.
//!
//! A chip echoes the key of the work it holds, and a driver that writes
//! the chain only when the header changes leaves every chip echoing one
//! key for as long as a pool job lasts. So a key names an assignment,
//! not a work tick: a tick that repeats the whole assignment the last
//! key went out under reuses that key, and the keys advance with the
//! work. Taking a key per tick retired the slot a whole chain was still
//! reporting under once the counter came round, and every solution from
//! then until the chips took the next job verified against another
//! job's header.

#[cfg(test)]
#[path = "tests/registry.rs"]
mod tests;

use crate::consts::WORK_REGISTRY_SLOTS;
use stratum_client::WorkAssignment;
use ubos_logging::warn;

/// In-flight work for a given `job_key` slot. Holds the assignment so an
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
}

/// The assignments in flight, indexed by job key. The key is the slot
/// index: the driver maps it to and from the chip's echo field, so a
/// returned solution finds the assignment it was found on.
pub(crate) struct WorkRegistry {
    slots: [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
    /// Keys the driver's echo field can carry, `1..=WORK_REGISTRY_SLOTS`.
    job_slots: u32,
    next_key: u32,
    /// Key the last dispatch went out under, so a repeat of that
    /// assignment finds it. `None` before the first dispatch and after
    /// a reset.
    last_key: Option<u32>,
}

impl WorkRegistry {
    /// Size the key space to what the driver's echo field carries,
    /// within this registry's capacity.
    pub(crate) fn new(driver_slots: usize) -> Self {
        assert!(driver_slots >= 1, "BUG: driver reports zero job slots");
        if driver_slots > WORK_REGISTRY_SLOTS {
            warn!(
                "Driver offers {} job slots, registry truncates to {}",
                driver_slots, WORK_REGISTRY_SLOTS
            );
        }
        let job_slots = u32::try_from(driver_slots.min(WORK_REGISTRY_SLOTS))
            .expect("BUG: the registry capacity fits a u32");
        Self {
            slots: [const { None }; WORK_REGISTRY_SLOTS],
            job_slots,
            next_key: 0,
            last_key: None,
        }
    }

    /// Forget every assignment and start the keys over, for a chain
    /// that was reset and holds no work.
    pub(crate) fn reset(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.next_key = 0;
        self.last_key = None;
    }

    /// Record a dispatch and say which key it goes out under. A repeat
    /// of the assignment the last dispatch carried, the same pool slot
    /// and the same work, keeps that key; anything else takes the next
    /// one, retiring whatever that key named before.
    pub(crate) fn place(&mut self, assignment: WorkAssignment, session_slot: usize) -> u32 {
        // The whole assignment and not its work id: a session mints its
        // ids from zero, so a reconnect on the same pool slot offers id
        // zero again for a job that is not the one in flight, and the
        // key would then name the header the previous session was
        // hashing while the chips took the new one.
        let repeat = self.last_key.filter(|&key| {
            self.slots[key as usize].as_ref().is_some_and(|held| {
                held.session_slot == session_slot && held.assignment == assignment
            })
        });
        if let Some(key) = repeat {
            return key;
        }
        let key = self.next_key;
        self.slots[key as usize] = Some(WorkSlot {
            assignment,
            session_slot,
        });
        self.next_key = (key + 1) % self.job_slots;
        self.last_key = Some(key);
        key
    }

    /// The assignment a job key names. The key comes from the chip's
    /// echo field, so a stray reply can name a key past the registry or
    /// one that holds nothing; both read as absent.
    pub(crate) fn get(&self, job_key: u32) -> Option<&WorkSlot> {
        self.slots.get(job_key as usize).and_then(Option::as_ref)
    }
}
