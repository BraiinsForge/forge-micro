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

//! Tests for `publish_work`'s ntime-roll math and the job-acceptance
//! dedup logic that stands in for the ticker when it's disabled.
//!
//! `run_stratum_client_inner`'s ticker gating (the `Option<Ticker>` /
//! `core::future::pending()` select arm) is not covered here: it needs
//! a `select3` over an embassy executor to observe, which these
//! synchronous unit tests don't run. What's unit-testable and covered
//! instead is the two pieces that gating delegates to: the step math
//! `publish_work` performs per call, and the `last_published` dedup
//! comparison the `None` path uses to publish exactly once per job
//! (exercised directly below via `JobId` equality, matching the `if`
//! guard in `run_stratum_client_inner`).

use super::*;

use crate::asic_session::AsicSession;
use crate::state::StratumState;
use mining_protocol::{JobId, LazyMerkle, MiningClientVisitor, MiningJob};

const SAMPLE_PREV_HASH: [u8; 32] = [0x11; 32];
const SAMPLE_TARGET: [u8; 32] = [0x22; 32];
const SAMPLE_ROOT: [u8; 32] = [0x33; 32];

fn sample_job(id: u32) -> MiningJob<'static> {
    static PREV_HASH: [u8; 32] = SAMPLE_PREV_HASH;
    static TARGET: [u8; 32] = SAMPLE_TARGET;
    static ROOT: [u8; 32] = SAMPLE_ROOT;
    MiningJob {
        id: JobId(id),
        prev_hash: &PREV_HASH,
        target: &TARGET,
        version: 0x2000_0000,
        nbits: 0x1705_c739,
        ntime: 0x6000_0000,
        clean_jobs: false,
        extranonce_2_size: 0,
        merkle: LazyMerkle::Final(&ROOT),
    }
}

/// Publish `n` times against a single fresh job with a fixed `step`
/// and collect the resulting `ntime_offset` after each publish.
fn offsets(step: u32, n: usize) -> heapless::Vec<u32, 16> {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();

    let mut out = heapless::Vec::new();
    for _ in 0..n {
        publish_work(&mut state, asic.pool_handle(0), step);
        let offset = state.job_ring.current().expect("slot present").ntime_offset;
        out.push(offset).expect("fits capacity");
    }
    out
}

#[test]
fn rolls_one_second_per_publish() {
    // The ticker branch always passes step=1 regardless of the
    // configured period: the period spaces publishes, it does not
    // scale the roll, so ntime never outruns wall clock.
    assert_eq!(offsets(1, 4).as_slice(), &[1, 2, 3, 4]);
}

#[test]
fn advances_by_the_given_step() {
    // publish_work's math is generic over the step even though the
    // run loop only ever passes 0 (job-acceptance publish) or 1
    // (ticker publish).
    assert_eq!(offsets(3, 3).as_slice(), &[3, 6, 9]);
}

#[test]
fn offset_caps_at_ntime_roll_max_secs() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();

    // A period large enough that two ticks would overshoot the cap.
    let period = NTIME_ROLL_MAX_SECS / 2 + 100;
    publish_work(&mut state, asic.pool_handle(0), period);
    publish_work(&mut state, asic.pool_handle(0), period);
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        NTIME_ROLL_MAX_SECS
    );
}

#[test]
fn zero_step_leaves_offset_unchanged() {
    // The job-acceptance publish (no ticker running) passes step=0:
    // ntime stays pinned to the job's base.
    assert_eq!(offsets(0, 5).as_slice(), &[0, 0, 0, 0, 0]);
}

#[test]
fn offset_resets_on_new_job() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();

    publish_work(&mut state, asic.pool_handle(0), 1);
    publish_work(&mut state, asic.pool_handle(0), 1);
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        2
    );

    state.visit_new_job(sample_job(2));
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        0
    );

    publish_work(&mut state, asic.pool_handle(0), 1);
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        1
    );
}

/// Mirrors the `last_published != Some(current_job_id)` guard in
/// `run_stratum_client_inner`'s `Either3::First` arm: with no ticker,
/// that comparison is the only thing deciding whether a given
/// `client.accept()` triggers a `publish_work` call. `JobId`'s
/// `Copy + PartialEq` derive is what makes the comparison viable
/// without borrowing the ring.
#[test]
fn last_published_dedup_fires_once_per_distinct_job() {
    fn accept(current_job_id: JobId, last_published: &mut Option<JobId>, publishes: &mut u32) {
        if *last_published != Some(current_job_id) {
            *publishes += 1;
            *last_published = Some(current_job_id);
        }
    }

    let mut last_published: Option<JobId> = None;
    let mut publishes = 0;

    // Same job accepted repeatedly (e.g. repeated `client.drive()`
    // wakeups with no new job in between) publishes only the first time.
    accept(JobId(1), &mut last_published, &mut publishes);
    accept(JobId(1), &mut last_published, &mut publishes);
    accept(JobId(1), &mut last_published, &mut publishes);
    assert_eq!(publishes, 1);

    // A distinct job publishes again exactly once.
    accept(JobId(2), &mut last_published, &mut publishes);
    accept(JobId(2), &mut last_published, &mut publishes);
    assert_eq!(publishes, 2);
}
