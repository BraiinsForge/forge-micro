//! Tests for `target_from_diff`.

use super::*;

#[test]
fn diff_one_yields_truediffone_in_le() {
    let target = target_from_diff(1.0);
    // truediffone = 0xFFFF * 2^208. In LE bytes that's 26 zero bytes,
    // then 0xFF 0xFF, then four zero bytes (the top of the 256-bit word).
    let mut expected = [0u8; 32];
    expected[26] = 0xFF;
    expected[27] = 0xFF;
    assert_eq!(target, expected);
}

#[test]
fn higher_diff_gives_smaller_target() {
    let easy = target_from_diff(1.0);
    let hard = target_from_diff(1024.0);
    // Higher difficulty means the most-significant set byte of the LE
    // target shifts down (smaller integer).
    let easy_high = easy.iter().rposition(|byte| *byte != 0).unwrap_or(0);
    let hard_high = hard.iter().rposition(|byte| *byte != 0).unwrap_or(0);
    assert!(hard_high <= easy_high, "easy={easy_high} hard={hard_high}");
}

#[test]
fn non_positive_diff_yields_max_target() {
    assert_eq!(target_from_diff(0.0), [0xFFu8; 32]);
    assert_eq!(target_from_diff(-1.0), [0xFFu8; 32]);
}
