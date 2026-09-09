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

use super::*;

fn assignment(work_id: u32) -> WorkAssignment {
    WorkAssignment {
        work_id,
        prev_hash: [0; 32],
        merkle_root: [0; 32],
        target: [0; 32],
        version: 0,
        nbits: 0,
        ntime: 0,
        version_mask: 0,
    }
}

#[test]
fn a_repeat_of_the_work_in_flight_keeps_its_key() {
    let mut registry = WorkRegistry::new(4);
    assert_eq!(registry.place(assignment(10), 0), 0);
    assert_eq!(registry.place(assignment(10), 0), 0);
    assert!(registry.get(1).is_none(), "the repeat took no key");
}

#[test]
fn new_work_takes_the_next_key() {
    let mut registry = WorkRegistry::new(4);
    assert_eq!(registry.place(assignment(10), 0), 0);
    assert_eq!(registry.place(assignment(11), 0), 1);
    assert_eq!(
        registry.get(0).map(|slot| slot.assignment.work_id),
        Some(10)
    );
    assert_eq!(
        registry.get(1).map(|slot| slot.assignment.work_id),
        Some(11)
    );
}

#[test]
fn a_session_that_restarts_its_work_ids_brings_other_work() {
    let mut registry = WorkRegistry::new(4);
    assert_eq!(registry.place(assignment(0), 0), 0);
    // A reconnect mints the work ids from zero again, so the id alone
    // cannot tell the new pool job from the one still in flight.
    let mut reconnected = assignment(0);
    reconnected.merkle_root = [1; 32];
    assert_eq!(registry.place(reconnected, 0), 1);
    assert_eq!(
        registry.get(1).map(|slot| slot.assignment.merkle_root),
        Some([1; 32])
    );
}

#[test]
fn the_same_work_id_from_another_pool_is_other_work() {
    let mut registry = WorkRegistry::new(4);
    assert_eq!(registry.place(assignment(10), 0), 0);
    assert_eq!(registry.place(assignment(10), 1), 1);
    assert_eq!(registry.get(1).map(|slot| slot.session_slot), Some(1));
}

#[test]
fn only_the_last_dispatch_counts_as_in_flight() {
    let mut registry = WorkRegistry::new(4);
    assert_eq!(registry.place(assignment(10), 0), 0);
    assert_eq!(registry.place(assignment(11), 0), 1);
    assert_eq!(registry.place(assignment(10), 0), 2);
}

#[test]
fn keys_wrap_at_the_drivers_slot_count() {
    let mut registry = WorkRegistry::new(2);
    assert_eq!(registry.place(assignment(1), 0), 0);
    assert_eq!(registry.place(assignment(2), 0), 1);
    assert_eq!(registry.place(assignment(3), 0), 0);
    assert_eq!(registry.get(0).map(|slot| slot.assignment.work_id), Some(3));
}

#[test]
fn the_slot_count_is_clamped_to_the_capacity() {
    let registry = WorkRegistry::new(WORK_REGISTRY_SLOTS * 4);
    assert_eq!(registry.job_slots as usize, WORK_REGISTRY_SLOTS);
}

#[test]
fn a_key_past_the_registry_names_nothing() {
    let registry = WorkRegistry::new(4);
    assert!(registry.get(u32::MAX).is_none());
}

#[test]
fn reset_starts_the_keys_over() {
    let mut registry = WorkRegistry::new(4);
    assert_eq!(registry.place(assignment(10), 0), 0);
    assert_eq!(registry.place(assignment(11), 0), 1);
    registry.reset();
    assert!(registry.get(0).is_none());
    assert!(registry.get(1).is_none());
    // The work in flight before the reset is not in flight after it.
    assert_eq!(registry.place(assignment(11), 0), 0);
}
