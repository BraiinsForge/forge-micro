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

//! Tests for the pure helpers behind [`super::record_share`].
//! `record_share` itself is coupled to [`embassy_time::Instant`] and
//! the module-level statics, so the tests exercise the extracted
//! `hashes_per_share` and `next_ewma` functions instead.

use super::{
    BUCKET_MS, EWMA_ALPHA_SHIFT, MAX_DECAY_STEPS, decay_silent, hashes_per_share, next_ewma,
};

fn target_with_leading_zero_bytes(zero_bytes: usize) -> [u8; 32] {
    let mut target = [0u8; 32];
    let nonzero_index = 31 - zero_bytes;
    target[nonzero_index] = 0x01;
    target
}

#[test]
fn hashes_per_share_target_one_byte() {
    // Target with the most significant byte set to 0x01 leaves
    // 7 leading zero bits in that byte → 256 hashes per share.
    let target = target_with_leading_zero_bytes(0);
    assert_eq!(hashes_per_share(&target), 1u64 << 7);
}

#[test]
fn hashes_per_share_scales_with_target() {
    // Each leading zero byte multiplies hashes-per-share by 256.
    assert_eq!(
        hashes_per_share(&target_with_leading_zero_bytes(1)),
        1u64 << 15
    );
    assert_eq!(
        hashes_per_share(&target_with_leading_zero_bytes(2)),
        1u64 << 23
    );
}

#[test]
fn hashes_per_share_saturates_when_shift_overflows() {
    // Eight leading zero bytes leaves 0x01 at index 23: 7 + 8*8 = 71
    // bits to shift by, which overflows a u64; the checked_shl path
    // saturates to u64::MAX.
    let target = target_with_leading_zero_bytes(8);
    assert_eq!(hashes_per_share(&target), u64::MAX);
}

#[test]
fn hashes_per_share_all_zero_target_saturates() {
    // An all-zero target counts 256 leading zero bits and saturates.
    assert_eq!(hashes_per_share(&[0u8; 32]), u64::MAX);
}

#[test]
fn next_ewma_from_zero_takes_one_alpha_step() {
    // First finalized bucket: previous = 0, so the new value is just
    // bucket_rate >> EWMA_ALPHA_SHIFT.
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
    // Bucket rate equal to the previous EWMA leaves the value
    // unchanged: delta is zero.
    assert_eq!(next_ewma(10_000, 1_000, 10_000), 10_000);
}

#[test]
fn next_ewma_decays_toward_lower_rate() {
    // previous = 8000, bucket rate = 0 → delta = -8000, shift right
    // by 3 → -1000 → next = 7000.
    assert_eq!(next_ewma(0, 1_000, 8_000), 7_000);
}

#[test]
fn next_ewma_saturates_on_huge_bucket() {
    // Bucket total large enough that bucket_total * 1000 overflows
    // u64; saturating_mul caps the rate at u64::MAX and the alpha
    // shift folds it into the EWMA without further overflow.
    assert_eq!(next_ewma(u64::MAX, 1, 0), u64::MAX >> EWMA_ALPHA_SHIFT);
}

#[test]
fn decay_reaches_zero() {
    // 64 consecutive zero-rate EWMA steps from 8000 must reach 0.
    let mut value: u64 = 8_000;
    for _ in 0..MAX_DECAY_STEPS {
        value = next_ewma(0, BUCKET_MS, value);
    }
    assert_eq!(value, 0);
}

#[test]
fn decay_partial_steps() {
    // Three missed zero-rate buckets from 8000 reduce the EWMA in a
    // predictable cascade: 8000 → 7000 → 6125 → 5359.
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
    // Steady-state EWMA, then 30 s of silence, then a low-rate share.
    let steady: u64 = 8_000;
    let elapsed_ms: u64 = 30 * BUCKET_MS;
    let bucket_total: u64 = 32; // ~Diff-1 share, very low

    // Pre-resume: what hashrate() returns the moment before the share.
    let read_missed = (elapsed_ms - BUCKET_MS) / BUCKET_MS;
    let displayed_before = decay_silent(steady, read_missed);

    // Post-resume: what record_share now writes on the finalising share.
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
