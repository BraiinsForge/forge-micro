//! Tests for block-header construction against known-good vectors.
//!
//! Vectors are taken from the reference-miner unit tests (`test_mining.c` /
//! "Test nonce diff checking") which run a redacted-derived verifier on
//! real-world stratum data. Matching their output proves the
//! byte-order handling of `version`/`prev_hash`/`ntime`/`nbits` is correct.

use crate::hashing::{build_header, hash_meets_target, header_hash};
use crate::hex::decode_to_slice;

fn hex32(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    assert!(decode_to_slice(hex.as_bytes(), &mut out), "bad hex literal");
    out
}

fn le256_to_f64(hash: &[u8; 32]) -> f64 {
    let mut value = 0.0f64;
    for byte in hash.iter().rev() {
        value = value * 256.0 + f64::from(*byte);
    }
    value
}

// 0x00000000FFFF0000_000000000000_0000000000000000_0000000000000000
const TRUE_DIFF_ONE: f64 =
    26_959_535_291_011_309_493_156_476_344_723_991_336_010_898_738_574_164_086_137_773_096_960.0;

// Diff-1 target: bytes 26..28 = 0xFF, all others zero (LE).
fn diff_one_target() -> [u8; 32] {
    let mut target = [0u8; 32];
    target[26] = 0xFF;
    target[27] = 0xFF;
    target
}

#[test]
fn build_header_matches_redacted_vector() {
    let version: u32 = 0x2000_0004;
    let prev_hash = hex32("d02b10fc0d4711eae1a805af50a8a83312a2215e00017f2b0000000000000000");
    let merkle_root = hex32("6d0359c451434605c52a5a9ce074340be47c2c63840731f9edf1db3f26b1cdd9");
    let ntime: u32 = 0x646F_F1A9;
    let nbits: u32 = 0x1705_AE3A;
    let nonce: u32 = 0x276E_8947;

    let header = build_header(version, prev_hash, merkle_root, ntime, nbits, nonce);

    assert_eq!(&header[0..4], &[0x04, 0x00, 0x00, 0x20]);

    let expected_prev = [
        0xfc, 0x10, 0x2b, 0xd0, 0xea, 0x11, 0x47, 0x0d, 0xaf, 0x05, 0xa8, 0xe1, 0x33, 0xa8, 0xa8,
        0x50, 0x5e, 0x21, 0xa2, 0x12, 0x2b, 0x7f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];
    assert_eq!(&header[4..36], &expected_prev);

    assert_eq!(&header[36..68], &merkle_root);
    assert_eq!(&header[68..72], &[0xa9, 0xf1, 0x6f, 0x64]);
    assert_eq!(&header[72..76], &[0x3a, 0xae, 0x05, 0x17]);
    assert_eq!(&header[76..80], &[0x47, 0x89, 0x6e, 0x27]);
}

#[test]
fn reconstructs_bitcoin_block_1() {
    // Bitcoin mainnet block height 1 (first block after genesis).
    // Known displayed hash: 00000000839a8e6886ab5951d76f4114...
    let version: u32 = 0x0000_0001;
    let prev_hash = hex32("0a8ce26f72b3f1b646a2a6c14ff763ae65831e939c085ae10019d66800000000");
    let merkle_root = hex32("982051fd1e4ba744bbbe680e1fee14677ba1a3c3540bf7b1cdb606e857233e0e");
    let ntime: u32 = 0x4966_BC61;
    let nbits: u32 = 0x1D00_FFFF;
    let nonce: u32 = 0x9962_E301;

    let hash = header_hash(version, prev_hash, merkle_root, ntime, nbits, nonce);

    let mut displayed = hash;
    displayed.reverse();
    let expected = hex32("00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048");
    assert_eq!(displayed, expected, "block 1 hash mismatch");

    let share_diff = TRUE_DIFF_ONE / le256_to_f64(&hash);
    assert!(
        share_diff >= 1.0,
        "share diff {share_diff} should be >= network diff 1",
    );
}

#[test]
fn hash_meets_target_accepts_block_1_at_diff_one() {
    let version: u32 = 0x0000_0001;
    let prev_hash = hex32("0a8ce26f72b3f1b646a2a6c14ff763ae65831e939c085ae10019d66800000000");
    let merkle_root = hex32("982051fd1e4ba744bbbe680e1fee14677ba1a3c3540bf7b1cdb606e857233e0e");
    let ntime: u32 = 0x4966_BC61;
    let nbits: u32 = 0x1D00_FFFF;
    let nonce: u32 = 0x9962_E301;

    let hash = header_hash(version, prev_hash, merkle_root, ntime, nbits, nonce);
    assert!(hash_meets_target(&hash, &diff_one_target()));
}

#[test]
fn header_hash_reports_redacted_diff() {
    // reference-miner asserts the share's difficulty truncates to 18.
    let version: u32 = 0x2000_0004;
    let prev_hash = hex32("d02b10fc0d4711eae1a805af50a8a83312a2215e00017f2b0000000000000000");
    let merkle_root = hex32("6d0359c451434605c52a5a9ce074340be47c2c63840731f9edf1db3f26b1cdd9");
    let ntime: u32 = 0x646F_F1A9;
    let nbits: u32 = 0x1705_AE3A;
    let nonce: u32 = 0x276E_8947;

    let hash = header_hash(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let diff = TRUE_DIFF_ONE / le256_to_f64(&hash);

    assert!(
        (17.0..19.0).contains(&diff),
        "expected diff ~18, got {diff} for hash {hash:?}",
    );
}
