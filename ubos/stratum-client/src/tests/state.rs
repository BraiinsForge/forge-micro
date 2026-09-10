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

//! Tests for `StratumState`'s `MiningClientVisitor` impl.

use super::*;

use crate::job_ring::JobSlot;
use mining_protocol::{JobId, LazyMerkle, MAX_EXTRANONCE_2, MiningJob, SessionUpdate};

const SAMPLE_PREV_HASH: [u8; 32] = [0x11; 32];
const SAMPLE_TARGET: [u8; 32] = [0x22; 32];
const SAMPLE_ROOT: [u8; 32] = [0x33; 32];

fn sample_job(id: u32, clean_jobs: bool) -> MiningJob<'static> {
    static PREV_HASH: [u8; 32] = SAMPLE_PREV_HASH;
    static TARGET: [u8; 32] = SAMPLE_TARGET;
    static ROOT: [u8; 32] = SAMPLE_ROOT;
    MiningJob {
        id: JobId(id),
        prev_hash: &PREV_HASH,
        target: &TARGET,
        version: 0x2000_0000,
        nbits: 0x1705_c739,
        ntime: 0x6044_9522,
        clean_jobs,
        extranonce_2_size: 0,
        merkle: LazyMerkle::Final(&ROOT),
    }
}

#[test]
fn visit_new_job_populates_slot() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1, false));

    let current = state.job_ring.current().expect("slot populated");
    assert_eq!(
        *current,
        JobSlot {
            job_id: JobId(1),
            valid: true,
            prev_hash: SAMPLE_PREV_HASH,
            target: SAMPLE_TARGET,
            merkle_root: SAMPLE_ROOT,
            version: 0x2000_0000,
            nbits: 0x1705_c739,
            ntime_base: 0x6044_9522,
            ntime_offset: 0,
            extranonce_2_size: 0,
            clean_jobs: false,
        }
    );
}

#[test]
fn visit_new_job_drops_when_en2_size_exceeds_max() {
    let mut state = StratumState::new(0);
    let mut job = sample_job(1, false);
    job.extranonce_2_size = u8::try_from(MAX_EXTRANONCE_2 + 1).expect("MAX_EXTRANONCE_2 fits u8");
    state.visit_new_job(job);
    assert!(state.job_ring.current().is_none(), "no slot allocated");
}

#[test]
fn clean_jobs_invalidates_previous_slot() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1, false));
    state.visit_new_job(sample_job(2, true));

    assert!(state.job_ring.is_valid(JobId(2)));
    assert!(!state.job_ring.is_valid(JobId(1)));
}

#[test]
fn slot_target_comes_from_job() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1, false));

    let current = state.job_ring.current().expect("slot present");
    assert_eq!(current.target, SAMPLE_TARGET);
}

#[test]
fn set_target_does_not_patch_current_slot() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1, false));

    let new_target: [u8; 32] = [0xAA; 32];
    state.visit_session_update(SessionUpdate::SetTarget(&new_target));

    let current = state.job_ring.current().expect("slot present");
    assert_eq!(current.target, SAMPLE_TARGET);
}

/// Accepted shares recorded through a slot-1 `StratumState` must land in
/// slot 1's counter and be visible in the summed `shares_accepted()`,
/// while slot 0's counter stays untouched — the routing the per-slot
/// arrays exist for.
#[test]
fn accepted_shares_route_to_their_own_slot() {
    let baseline = [
        crate::metrics::for_slot(0)
            .expect("slot 0 in range")
            .shares_accepted(),
        crate::metrics::for_slot(1)
            .expect("slot 1 in range")
            .shares_accepted(),
    ];

    let mut state = StratumState::new(1);
    state.visit_submit_result(SubmitResult::Accepted { count: 3, work: 1 });

    assert_eq!(
        crate::metrics::for_slot(1)
            .expect("slot 1 in range")
            .shares_accepted(),
        baseline[1] + 3,
        "slot 1's counter records the accepted shares"
    );
    assert_eq!(
        crate::metrics::for_slot(0)
            .expect("slot 0 in range")
            .shares_accepted(),
        baseline[0],
        "slot 0's counter is untouched by slot 1's shares"
    );
    assert_eq!(
        shares_accepted(),
        baseline[0] + baseline[1] + 3,
        "the summed accessor includes slot 1's shares"
    );
}
