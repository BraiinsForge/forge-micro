//! Tests for the pure helpers behind [`super::record_share`].
//! `record_share` itself is coupled to [`embassy_time::Instant`] and
//! the module-level statics, so the tests exercise the extracted
//! `hashes_per_share` and `next_ewma` functions instead.

use super::{hashes_per_share, next_ewma, EWMA_ALPHA_SHIFT};

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
    let bucket_rate: u32 = 8_000;
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
    // u64; saturating_mul caps it and the cast to u32 saturates too.
    assert_eq!(next_ewma(u64::MAX, 1, 0), u32::MAX >> EWMA_ALPHA_SHIFT);
}
