//! Tests for `StratumState`'s `MiningClientVisitor` impl.

use super::*;

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
        version_rolling_mask: 0,
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
    assert_eq!(current.job_id, JobId(1));
    assert_eq!(current.merkle_root, SAMPLE_ROOT);
    assert_eq!(current.prev_hash, SAMPLE_PREV_HASH);
    assert_eq!(current.version, 0x2000_0000);
    assert!(current.valid);
}

#[test]
fn clean_jobs_invalidates_previous_slot() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1, false));
    state.visit_new_job(sample_job(2, true));

    assert!(state.job_ring.is_valid(JobId(2)));
    assert!(!state.job_ring.is_valid(JobId(1)));
    assert!(state.clean_jobs_pending);
}

#[test]
fn set_target_updates_state_and_current_slot() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1, false));

    let new_target: [u8; 32] = [0xAA; 32];
    state.visit_session_update(SessionUpdate::SetTarget(&new_target));

    assert_eq!(state.target, new_target);
    let current = state.job_ring.current().expect("slot present");
    assert_eq!(current.target, new_target);
}

#[test]
fn set_version_mask_updates_state_and_current_slot() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1, false));
    state.visit_session_update(SessionUpdate::SetVersionMask(0x1fff_e000));

    assert_eq!(state.version_mask, 0x1fff_e000);
    let current = state.job_ring.current().expect("slot present");
    assert_eq!(current.version_rolling_mask, 0x1fff_e000);
}

#[test]
fn set_extranonce_records_prefix_and_size() {
    let mut state = StratumState::new();
    let prefix = [0xde, 0xad, 0xbe, 0xef];
    state.visit_session_update(SessionUpdate::SetExtranonce {
        prefix: &prefix,
        en2_size: 4,
    });

    assert_eq!(state.extranonce_prefix_len, 4);
    assert_eq!(&state.extranonce_prefix[..4], &prefix);
    assert_eq!(state.extranonce_2_size, 4);
}
