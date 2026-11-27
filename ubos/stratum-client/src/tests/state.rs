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
use crate::metrics::{PoolCountersHandle, PoolMetricsSnapshot};
use rusty_fork::rusty_fork_test;

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

rusty_fork_test! {
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
}

rusty_fork_test! {
    #[test]
    fn visit_new_job_drops_when_en2_size_exceeds_max() {
        let mut state = StratumState::new(0);
        let mut job = sample_job(1, false);
        job.extranonce_2_size = u8::try_from(MAX_EXTRANONCE_2 + 1).expect("MAX_EXTRANONCE_2 fits u8");
        state.visit_new_job(job);
        assert!(state.job_ring.current().is_none(), "no slot allocated");
    }
}

rusty_fork_test! {
    #[test]
    fn clean_jobs_invalidates_previous_slot() {
        let mut state = StratumState::new(0);
        state.visit_new_job(sample_job(1, false));
        state.visit_new_job(sample_job(2, true));

        assert!(state.job_ring.is_valid(JobId(2)));
        assert!(!state.job_ring.is_valid(JobId(1)));
    }
}

rusty_fork_test! {
    #[test]
    fn slot_target_comes_from_job() {
        let mut state = StratumState::new(0);
        state.visit_new_job(sample_job(1, false));

        let current = state.job_ring.current().expect("slot present");
        assert_eq!(current.target, SAMPLE_TARGET);
    }
}

rusty_fork_test! {
    #[test]
    fn set_target_does_not_patch_current_slot() {
        let mut state = StratumState::new(0);
        state.visit_new_job(sample_job(1, false));

        let new_target: [u8; 32] = [0xAA; 32];
        state.visit_session_update(SessionUpdate::SetTarget(&new_target));

        let current = state.job_ring.current().expect("slot present");
        assert_eq!(current.target, SAMPLE_TARGET);
    }
}

rusty_fork_test! {
    #[test]
    fn accepted_shares_route_to_their_own_slot() {
        let counters = PoolCountersHandle::take();

        let mut state = StratumState::new(1);
        state.visit_submit_result(SubmitResult::Accepted { count: 3, work: 1 });

        assert_eq!(
            crate::metrics::for_slot(1)
                .expect("slot 1 in range")
                .shares_accepted(),
            3,
            "slot 1's counter records the accepted shares"
        );
        assert_eq!(
            crate::metrics::for_slot(0)
                .expect("slot 0 in range")
                .shares_accepted(),
            0,
            "slot 0's counter is untouched by slot 1's shares"
        );
        assert_eq!(
            counters.shares_accepted(),
            3,
            "the summed accessor includes slot 1's shares"
        );
    }
}

rusty_fork_test! {
    #[test]
    fn a_pool_face_counts_in_its_own_table() {
        let counters = PoolCountersHandle::take();
        let session: crate::asic_session::AsicSession<2> = crate::asic_session::AsicSession::new();

        session.pool_handle(1).set_alive(true);

        assert_eq!(counters.pool_alive_bitmap(), 0b10);
    }
}

rusty_fork_test! {
    // A restarted session keeps counting assignment ids where the previous
    // one stopped, so a solution queued for the old work finds no match.
    #[test]
    fn restart_keeps_assignment_ids_unique() {
        let mut state = StratumState::new(1);
        state.visit_new_job(sample_job(1, false));
        let asic: crate::asic_session::AsicSession<2> = crate::asic_session::AsicSession::new();
        for _ in 0..3 {
            state.publish_work(asic.pool_handle(1), 0);
        }

        state.restart();
        assert_eq!(state.next_assignment_id, 3);
        assert!(state.job_ring.current().is_none());
        for old_id in 0..3 {
            assert!(state.work_ring.lookup(old_id).is_none());
        }

        state.visit_new_job(sample_job(2, false));
        state.publish_work(asic.pool_handle(1), 0);
        assert!(state.work_ring.lookup(0).is_none());
        assert!(state.work_ring.lookup(3).is_some());
    }
}

rusty_fork_test! {
    #[test]
    fn handle_reads_the_table_it_holds() {
        let handle = PoolCountersHandle::take();
        let pools = &crate::metrics::POOL_METRICS;
        assert_eq!(handle.pool_alive_bitmap(), 0);
        assert_eq!(handle.shares_accepted(), 0);
        assert_eq!(handle.shares_rejected(), 0);

        pools[2].set_alive(true);
        pools[7].set_alive(true);
        pools[1].record_shares_accepted(3, 30);
        pools[5].record_shares_accepted(4, 40);
        pools[5].record_share_rejected(5);

        assert_eq!(handle.pool_alive_bitmap(), 0b1000_0100);
        assert_eq!(handle.shares_accepted(), 7);
        assert_eq!(handle.shares_rejected(), 1);
    }
}

rusty_fork_test! {
    #[test]
    fn results_add_their_work_to_the_matching_sum() {
        let mut state = StratumState::new(2);
        state.visit_submit_result(SubmitResult::Accepted { count: 1, work: 1024 });
        state.visit_submit_result(SubmitResult::Accepted { count: 2, work: 3000 });
        state.visit_submit_result(SubmitResult::Rejected {
            error_code: None,
            error_message: None,
            work: 512,
        });

        let snapshot = crate::metrics::for_slot(2).expect("slot 2 in range").snapshot();
        assert_eq!(snapshot.work_accepted.get(), 4024);
        assert_eq!(snapshot.work_rejected.get(), 512);
        assert_eq!(snapshot.shares_accepted, 3);
        assert_eq!(snapshot.shares_rejected, 1);
    }
}

rusty_fork_test! {
    #[test]
    fn set_target_changes_only_the_difficulty() {
        let mut target = [0u8; 32];
        // 0xffff_0000 / 0x0007_fff8 = 8192.
        target[24..28].copy_from_slice(&0x0007_fff8_u32.to_le_bytes());

        let mut state = StratumState::new(3);
        let before = crate::metrics::for_slot(3).expect("slot 3 in range").snapshot();
        state.visit_session_update(SessionUpdate::SetTarget(&target));
        let after = crate::metrics::for_slot(3).expect("slot 3 in range").snapshot();

        assert_eq!(after.difficulty.get(), 8192);
        assert_eq!(
            PoolMetricsSnapshot { difficulty: before.difficulty, ..after },
            before,
        );
        assert_eq!(crate::metrics::for_slot(0).expect("slot 0 in range").snapshot().difficulty.get(), 0);
    }
}

rusty_fork_test! {
    #[test]
    fn clear_difficulty_zeroes_only_the_cleared_slot() {
        let session: crate::asic_session::AsicSession<2> = crate::asic_session::AsicSession::new();
        for slot in 0..2 {
            crate::metrics::for_slot(slot).expect("slot in range").set_difficulty(8192);
        }

        session.pool_handle(0).clear_difficulty();

        assert_eq!(crate::metrics::for_slot(0).expect("slot 0 in range").snapshot().difficulty.get(), 0);
        assert_eq!(crate::metrics::for_slot(1).expect("slot 1 in range").snapshot().difficulty.get(), 8192);
    }
}
