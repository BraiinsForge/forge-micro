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

//! Tests for the pure helpers behind `record_share`, which itself is
//! coupled to [`embassy_time::Instant`].

use super::{
    BUCKET_MS, EWMA_ALPHA_SHIFT, MAX_DECAY_STEPS, decay_silent, hashes_per_solution, next_ewma,
    resume_split,
};

fn target_with_leading_zero_bytes(zero_bytes: usize) -> [u8; 32] {
    let mut target = [0u8; 32];
    let nonzero_index = 31 - zero_bytes;
    target[nonzero_index] = 0x01;
    target
}

#[test]
fn hashes_per_solution_target_one_byte() {
    let target = target_with_leading_zero_bytes(0);
    assert_eq!(hashes_per_solution(&target), 1u64 << 7);
}

#[test]
fn hashes_per_solution_scales_with_target() {
    assert_eq!(
        hashes_per_solution(&target_with_leading_zero_bytes(1)),
        1u64 << 15
    );
    assert_eq!(
        hashes_per_solution(&target_with_leading_zero_bytes(2)),
        1u64 << 23
    );
}

#[test]
fn hashes_per_solution_saturates_when_shift_overflows() {
    let target = target_with_leading_zero_bytes(8);
    assert_eq!(hashes_per_solution(&target), u64::MAX);
}

#[test]
fn hashes_per_solution_all_zero_target_saturates() {
    assert_eq!(hashes_per_solution(&[0u8; 32]), u64::MAX);
}

#[test]
fn next_ewma_from_zero_takes_one_alpha_step() {
    let bucket_total: u64 = 8_000;
    let elapsed_ms: u64 = 1_000;
    let bucket_rate: u64 = 8_000;
    assert_eq!(
        next_ewma(bucket_total, elapsed_ms, 0),
        bucket_rate >> EWMA_ALPHA_SHIFT
    );
}

#[test]
fn next_ewma_steady_state_holds() {
    assert_eq!(next_ewma(10_000, 1_000, 10_000), 10_000);
}

#[test]
fn next_ewma_decays_toward_lower_rate() {
    assert_eq!(next_ewma(0, 1_000, 8_000), 7_000);
}

#[test]
fn next_ewma_saturates_on_huge_bucket() {
    assert_eq!(next_ewma(u64::MAX, 1, 0), u64::MAX >> EWMA_ALPHA_SHIFT);
}

#[test]
fn decay_reaches_zero() {
    let mut value: u64 = 8_000;
    for _ in 0..MAX_DECAY_STEPS {
        value = next_ewma(0, BUCKET_MS, value);
    }
    assert_eq!(value, 0);
}

#[test]
fn decay_partial_steps() {
    let start: u64 = 8_000;
    let once = next_ewma(0, BUCKET_MS, start);
    let twice = next_ewma(0, BUCKET_MS, once);
    let thrice = next_ewma(0, BUCKET_MS, twice);
    assert_eq!(once, 7_000);
    assert_eq!(twice, 6_125);
    assert_eq!(thrice, 5_359);
}

#[test]
fn record_share_resume_does_not_exceed_displayed_value() {
    let steady: u64 = 8_000;
    let elapsed_ms: u64 = 30 * BUCKET_MS;
    let bucket_total: u64 = 32; // ~Diff-1 share, very low

    let read_missed = (elapsed_ms - BUCKET_MS) / BUCKET_MS;
    let displayed_before = decay_silent(steady, read_missed);

    let silent_buckets = (elapsed_ms - BUCKET_MS) / BUCKET_MS;
    let decayed = decay_silent(steady, silent_buckets);
    let stored_after = next_ewma(bucket_total, BUCKET_MS, decayed);

    // The new stored value must not exceed the decayed read by more than
    // one α-step's worth of the incoming bucket rate.
    assert!(
        stored_after <= displayed_before + (bucket_total / 8 + 1),
        "post-resume stored {stored_after} jumped above pre-resume displayed {displayed_before}",
    );
}

#[test]
fn resume_split_below_two_buckets_folds_the_whole_gap() {
    for elapsed in [1, BUCKET_MS, 2 * BUCKET_MS - 1] {
        assert_eq!(resume_split(elapsed), (0, elapsed), "elapsed {elapsed}");
    }
}

#[test]
fn resume_split_folds_over_the_true_remainder() {
    let half = BUCKET_MS / 2;
    assert_eq!(resume_split(2 * BUCKET_MS + half), (1, BUCKET_MS + half));
    assert_eq!(resume_split(5 * BUCKET_MS + half), (4, BUCKET_MS + half));
}

#[test]
fn resume_split_on_an_exact_multiple_folds_one_bucket() {
    for buckets in 2..8u64 {
        assert_eq!(
            resume_split(buckets * BUCKET_MS),
            (buckets - 1, BUCKET_MS),
            "{buckets} buckets"
        );
    }
}

#[test]
fn resume_split_never_returns_a_zero_window() {
    for elapsed in (1..40u64).map(|n| n * BUCKET_MS / 3) {
        let (_, window) = resume_split(elapsed);
        assert!(window > 0, "elapsed {elapsed} produced a zero fold window");
    }
}
