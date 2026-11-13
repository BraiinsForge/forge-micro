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

//! Tests for the per-slot channel pairs.
//!
//! The dispatcher-side behavior (the scheduler picking which slot's
//! cell to read on a work tick) lives in `ubos-mining` and is covered
//! there; what is testable here is the routing contract the
//! dispatcher relies on: slot isolation in both directions and
//! teardown clearing.

use super::*;
use rusty_fork::rusty_fork_test;

fn assignment(assignment_id: u32) -> WorkAssignment {
    WorkAssignment {
        assignment_id,
        prev_hash: [0x11; 32],
        merkle_root: [0x33; 32],
        target: [0x22; 32],
        version: 0x2000_0000,
        nbits: 0x1705_c739,
        ntime: 0x6000_0000,
        version_mask: 0,
    }
}

fn solution(assignment_id: u32) -> Solution {
    Solution {
        assignment_id,
        nonce: 0xdead_beef,
        ntime: 0x6000_0001,
        version_bits: 0,
    }
}

rusty_fork_test! {
    #[test]
    fn work_publishes_stay_in_their_slot() {
        let session: AsicSession<3> = AsicSession::new();
        session
            .pool_handle(0)
            .publish_work(assignment(100));
        session
            .pool_handle(2)
            .publish_work(assignment(200));

        let mut receiver_0 = session.work_receiver(0).expect("receiver slot free");
        let mut receiver_1 = session.work_receiver(1).expect("receiver slot free");
        let mut receiver_2 = session.work_receiver(2).expect("receiver slot free");

        assert_eq!(
            receiver_0
                .try_get()
                .expect("slot 0 published")
                .assignment_id,
            100
        );
        assert!(receiver_1.try_get().is_none());
        assert_eq!(
            receiver_2
                .try_get()
                .expect("slot 2 published")
                .assignment_id,
            200
        );
    }
}

rusty_fork_test! {
    #[test]
    fn work_cell_is_latest_wins_per_slot() {
        let session: AsicSession<2> = AsicSession::new();
        let handle = session.pool_handle(1);
        handle.publish_work(assignment(1));
        handle.publish_work(assignment(2));

        let mut receiver = session.work_receiver(1).expect("receiver slot free");
        assert_eq!(receiver.try_get().expect("published").assignment_id, 2);
    }
}

rusty_fork_test! {
    #[test]
    fn clear_work_empties_only_the_cleared_slot() {
        let session: AsicSession<2> = AsicSession::new();
        session
            .pool_handle(0)
            .publish_work(assignment(1));
        session
            .pool_handle(1)
            .publish_work(assignment(2));

        session
            .pool_handle(0)
            .clear_work();

        let mut receiver_0 = session.work_receiver(0).expect("receiver slot free");
        let mut receiver_1 = session.work_receiver(1).expect("receiver slot free");
        assert!(receiver_0.try_get().is_none());
        assert_eq!(receiver_1.try_get().expect("untouched").assignment_id, 2);
    }
}

rusty_fork_test! {
    #[test]
    fn solutions_route_to_the_submitted_slot() {
        let session: AsicSession<3> = AsicSession::new();
        assert!(session.submit_solution(1, solution(7)));

        assert!(
            session
                .pool_handle(0)
                .try_take_solution()
                .is_none()
        );
        assert!(
            session
                .pool_handle(2)
                .try_take_solution()
                .is_none()
        );
        let routed = session
            .pool_handle(1)
            .try_take_solution()
            .expect("routed to slot 1");
        assert_eq!(routed.assignment_id, 7);
        // Drained: the queue holds no second copy.
        assert!(
            session
                .pool_handle(1)
                .try_take_solution()
                .is_none()
        );
    }
}

rusty_fork_test! {
    #[test]
    fn submit_solution_reports_full_queue() {
        let session: AsicSession<1> = AsicSession::new();
        for i in 0..SOLUTION_QUEUE_DEPTH {
            assert!(session.submit_solution(0, solution(u32::try_from(i).expect("small"))));
        }
        assert!(!session.submit_solution(0, solution(99)));
    }
}

#[test]
#[should_panic(expected = "pool slot out of range")]
fn pool_handle_rejects_out_of_range_slot() {
    let session: AsicSession<2> = AsicSession::new();
    let _ = session.pool_handle(2);
}

rusty_fork_test! {
    #[test]
    fn a_slot_hands_out_one_work_receiver_per_chain_and_no_more() {
        const CHAINS: usize = 3;
        let session: AsicSession<1, CHAINS> = AsicSession::new();
        let receivers: heapless::Vec<_, CHAINS> = (0..CHAINS)
            .map(|_| {
                session
                    .work_receiver(0)
                    .expect("receiver within the chain budget")
            })
            .collect();
        assert_eq!(receivers.len(), CHAINS);
        assert!(
            session.work_receiver(0).is_none(),
            "a receiver past the chain budget must be refused"
        );
    }
}
