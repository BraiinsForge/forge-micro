//! Tests for `publish_work`'s ntime-roll math.

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

/// Publish `n` times against a single fresh job and collect the
/// resulting `ntime_offset` after each publish.
fn offsets(ntime_roll_period_secs: Option<u32>, n: usize) -> heapless::Vec<u32, 16> {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1));
    let asic = AsicSession::new();

    let mut out = heapless::Vec::new();
    for _ in 0..n {
        publish_work(&mut state, &asic, ntime_roll_period_secs);
        let offset = state.job_ring.current().expect("slot present").ntime_offset;
        out.push(offset).expect("fits capacity");
    }
    out
}

#[test]
fn rolls_one_second_per_publish_at_period_1() {
    // Some(1) reproduces the pre-refactor unconditional +1/publish
    // behavior exactly, including the first publish carrying base+1.
    assert_eq!(offsets(Some(1), 4).as_slice(), &[1, 2, 3, 4]);
}

#[test]
fn rolls_slower_at_a_longer_period() {
    // period=3 rolls one second forward per 3 publishes: elapsed
    // 1,2,3,4,5,6 -> offset 0,0,1,1,1,2.
    assert_eq!(offsets(Some(3), 6).as_slice(), &[0, 0, 1, 1, 1, 2]);
}

#[test]
fn pins_ntime_to_base_when_period_is_none() {
    assert_eq!(offsets(None, 5).as_slice(), &[0, 0, 0, 0, 0]);
}

#[test]
fn zero_period_is_clamped_to_one_second() {
    // Some(0) must not be treated as None: it rolls as fast as the
    // publish cadence allows (period clamped to 1), so after one
    // publish the offset is elapsed_secs / 1 = 1.
    assert_eq!(offsets(Some(0), 1).as_slice(), &[1]);
}

#[test]
fn offset_and_elapsed_reset_on_new_job() {
    let mut state = StratumState::new();
    state.visit_new_job(sample_job(1));
    let asic = AsicSession::new();

    publish_work(&mut state, &asic, Some(1));
    publish_work(&mut state, &asic, Some(1));
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        2
    );

    state.visit_new_job(sample_job(2));
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        0
    );

    publish_work(&mut state, &asic, Some(1));
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        1
    );
}
