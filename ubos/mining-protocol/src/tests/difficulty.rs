use super::*;

/// Build an LE 32-byte target from an MSB-first hex literal of
/// exactly 64 chars. The first 32 input chars are the most
/// significant byte; the last are the least.
fn target_le_from_msb_hex(msb_hex: &str) -> [u8; 32] {
    assert_eq!(msb_hex.len(), 64);
    let mut be = [0u8; 32];
    assert!(crate::hex::decode_to_slice(msb_hex.as_bytes(), &mut be));
    let mut le = [0u8; 32];
    for (i, b) in be.iter().rev().enumerate() {
        le[i] = *b;
    }
    le
}

#[test]
fn difficulty_one_target_rounds_to_one() {
    // Bitcoin difficulty-1 target: 0x00000000FFFF0000 followed by 24 zero bytes.
    let target_le =
        target_le_from_msb_hex("00000000ffff0000000000000000000000000000000000000000000000000000");
    assert_eq!(approx_string(&target_le).as_str(), "1");
}

#[test]
fn pool_target_with_seven_leading_msb_zero_bytes() {
    // target_be[0..4] = 0x00000000, target_be[4..8] = 0x0007fff8 (the
    // braiins channel-open target we observed on the bench).
    let target_le =
        target_le_from_msb_hex("000000000007fff8000000000000000000000000000000000000000000000000");
    // 0xffff_0000 / 0x0007_fff8 = 4294901760 / 524280 = 8192
    assert_eq!(approx_string(&target_le).as_str(), "8192");
}

#[test]
fn easy_target_below_diff_one() {
    // top4 != 0 (target larger than the diff-1 target).
    let target_le =
        target_le_from_msb_hex("0001000000000000000000000000000000000000000000000000000000000000");
    assert_eq!(approx_string(&target_le).as_str(), "<1");
}

#[test]
fn very_hard_target_overflows_approximation() {
    // top4 = 0 and next4 = 0: the second u32 word is also zero so
    // the approximation cannot be expressed in u32.
    let target_le =
        target_le_from_msb_hex("0000000000000000000000010000000000000000000000000000000000000000");
    assert_eq!(approx_string(&target_le).as_str(), ">MAX");
}

#[test]
fn maximum_u32_difficulty() {
    // next4 = 1: maximum u32 diff = 0xffff_0000 = 4294901760.
    let target_le =
        target_le_from_msb_hex("0000000000000001000000000000000000000000000000000000000000000000");
    assert_eq!(approx_string(&target_le).as_str(), "4294901760");
}

#[test]
fn all_zero_target_yields_above_max() {
    // Edge case: top4 = 0 and next4 = 0.
    let target_le = [0u8; 32];
    assert_eq!(approx_string(&target_le).as_str(), ">MAX");
}

#[test]
fn all_ff_target_yields_below_one() {
    // Edge case: top4 = 0xffff_ffff → diff way below 1.
    let target_le = [0xffu8; 32];
    assert_eq!(approx_string(&target_le).as_str(), "<1");
}
