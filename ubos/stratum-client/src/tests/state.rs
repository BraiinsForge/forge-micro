//! Tests for `StratumState`'s `MiningClientVisitor` impl.

use super::*;

use crate::job_ring::JobSlot;
use mining_protocol::{JobId, LazyMerkle, MiningJob, SessionUpdate};

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
    let mut state = StratumState::new();
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
    let mut state = StratumState::new();
    let mut job = sample_job(1, false);
    job.extranonce_2_size = u8::try_from(MAX_EXTRANONCE_2 + 1).expect("MAX_EXTRANONCE_2 fits u8");
    state.visit_new_job(job);
    assert!(state.job_ring.current().is_none(), "no slot allocated");
}

#[test]
fn clean_jobs_invalidates_previous_slot() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1, false));
    state.visit_new_job(sample_job(2, true));

    assert!(state.job_ring.is_valid(JobId(2)));
    assert!(!state.job_ring.is_valid(JobId(1)));
}

#[test]
fn slot_target_comes_from_job() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1, false));

    let current = state.job_ring.current().expect("slot present");
    assert_eq!(current.target, SAMPLE_TARGET);
}

#[test]
fn set_target_does_not_patch_current_slot() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1, false));

    let new_target: [u8; 32] = [0xAA; 32];
    state.visit_session_update(SessionUpdate::SetTarget(&new_target));

    let current = state.job_ring.current().expect("slot present");
    assert_eq!(current.target, SAMPLE_TARGET);
}
