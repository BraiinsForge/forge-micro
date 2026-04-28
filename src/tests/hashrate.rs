//! Tests for the pure rate computation in [`super::compute_rate`].

use super::compute_rate;

#[test]
fn rate_at_target_one_byte() {
    // 10 shares over 1000 ms at target=1 (256 hashes/share) → 2560 H/s.
    assert_eq!(compute_rate(10, 1000, 1), 2560);
}

#[test]
fn rate_scales_with_target() {
    // Doubling the target by one byte multiplies hashes/share by 256.
    assert_eq!(compute_rate(1, 1000, 2), 65_536);
    assert_eq!(compute_rate(1, 1000, 3), 16_777_216);
}

#[test]
fn target_eight_saturates_to_u32_max() {
    // 1u64 << 64 would overflow; the checked_shl path saturates
    // hashes_per_share to u64::MAX and the cast to u32 then saturates.
    assert_eq!(compute_rate(1, 1000, 8), u32::MAX);
}

#[test]
fn very_high_target_still_saturates() {
    // 9 leading zero bytes is still beyond u64; saturation must hold.
    assert_eq!(compute_rate(1, 1000, 9), u32::MAX);
}

#[test]
fn enormous_delta_saturates_at_u32_max() {
    // 4 G shares over 1 ms at target=1 would multiply out past 2^32 H/s.
    assert_eq!(compute_rate(u64::MAX, 1, 1), u32::MAX);
}

#[test]
fn rate_rounds_down_on_partial_window() {
    // 1 share over 1500 ms at target=1 → 256 * 1000 / 1500 = 170 (truncated).
    assert_eq!(compute_rate(1, 1500, 1), 170);
}

#[test]
fn long_window_dilutes_rate() {
    // 1 share over 60 000 ms at target=1 → 256 / 60 = 4 H/s.
    assert_eq!(compute_rate(1, 60_000, 1), 4);
}
