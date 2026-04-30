//! Tests for block-header construction against known-good vectors.
//!
//! Vectors are taken from the reference-miner unit tests (`test_mining.c` /
//! "Test nonce diff checking") which run a redacted-derived verifier on
//! real-world stratum data. Matching their output proves the
//! byte-order handling of `version`/`prev_hash`/`ntime`/`nbits` is correct.

use super::{build_header, hash_meets_target, header_hash, target_from_diff};

fn hex32(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    decode(hex.as_bytes(), &mut out);
    out
}

fn decode(hex: &[u8], out: &mut [u8]) {
    assert_eq!(hex.len(), out.len() * 2, "hex length mismatch");
    for (index, byte) in out.iter_mut().enumerate() {
        let high = nibble(hex[index * 2]);
        let low = nibble(hex[index * 2 + 1]);
        *byte = (high << 4) | low;
    }
}

fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("bad hex nibble"),
    }
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

#[test]
fn build_header_matches_redacted_vector() {
    // Inputs: reference-miner test "Test nonce diff checking".
    // prev_block_hash/merkle_root are the stratum-format hex strings.
    let version: u32 = 0x2000_0004;
    let prev_hash = hex32("d02b10fc0d4711eae1a805af50a8a83312a2215e00017f2b0000000000000000");
    let merkle_root = hex32("6d0359c451434605c52a5a9ce074340be47c2c63840731f9edf1db3f26b1cdd9");
    let ntime: u32 = 0x646F_F1A9;
    let nbits: u32 = 0x1705_AE3A;
    let nonce: u32 = 0x276E_8947;

    let header = build_header(version, prev_hash, merkle_root, ntime, nbits, nonce);

    // Version u32 serialises to little-endian bytes in the header.
    assert_eq!(&header[0..4], &[0x04, 0x00, 0x00, 0x20]);

    // prev_hash: each 32-bit word reversed in place.
    let expected_prev = [
        0xfc, 0x10, 0x2b, 0xd0, 0xea, 0x11, 0x47, 0x0d, 0xaf, 0x05, 0xa8, 0xe1, 0x33, 0xa8, 0xa8,
        0x50, 0x5e, 0x21, 0xa2, 0x12, 0x2b, 0x7f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];
    assert_eq!(&header[4..36], &expected_prev);

    // merkle_root: untouched (we compute it locally in internal byte
    // order; the header stores it in the same order).
    assert_eq!(&header[36..68], &merkle_root);

    // ntime / nbits: stratum hex -> LE bytes.
    assert_eq!(&header[68..72], &[0xa9, 0xf1, 0x6f, 0x64]);
    assert_eq!(&header[72..76], &[0x3a, 0xae, 0x05, 0x17]);

    // nonce: LE bytes of the u32.
    assert_eq!(&header[76..80], &[0x47, 0x89, 0x6e, 0x27]);
}

#[test]
fn reconstructs_bitcoin_block_1() {
    // End-to-end check against a real Bitcoin mainnet block (height 1,
    // first block after genesis). The header fields are public and
    // anyone can cross-reference them on a block explorer.
    //
    // Inputs are in the exact shape a stratum miner receives them:
    //   - version/ntime/nbits as big-endian hex of the u32 value
    //   - prev_hash with its 32-bit words order-reversed (ckpool's
    //     swap_256 convention, see libckpool.h)
    //   - merkle_root in internal byte order (what calc_merkle_root
    //     would produce locally from coinbase + branches)
    //
    // Known hash (displayed):
    //   00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048
    let version: u32 = 0x0000_0001;
    let prev_hash = hex32("0a8ce26f72b3f1b646a2a6c14ff763ae65831e939c085ae10019d66800000000");
    let merkle_root = hex32("982051fd1e4ba744bbbe680e1fee14677ba1a3c3540bf7b1cdb606e857233e0e");
    let ntime: u32 = 0x4966_BC61; // 1231469665
    let nbits: u32 = 0x1D00_FFFF;
    let nonce: u32 = 0x9962_E301;

    let hash = header_hash(version, prev_hash, merkle_root, ntime, nbits, nonce);

    // The hash emerges in internal little-endian order; displayed form
    // is a full byte-reverse.
    let mut displayed = hash;
    displayed.reverse();
    let expected = hex32("00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048");
    assert_eq!(displayed, expected, "block 1 hash mismatch");

    // Block 1 came in at difficulty 1 exactly. A correct implementation
    // must report a share difficulty >= 1; historically miners produced
    // this block at diff ~1.99.
    let share_diff = TRUE_DIFF_ONE / le256_to_f64(&hash);
    assert!(
        share_diff >= 1.0,
        "share diff {share_diff} should be >= network diff 1",
    );
}

#[test]
fn target_from_diff_one_matches_truediff_one() {
    // Diff 1 target is 0x00000000FFFF0000_..._00 (mainnet diff-1).
    // In LE bytes: bytes 0..26 = 0, byte 26 = 0xFF, byte 27 = 0xFF, 28..31 = 0.
    let target = target_from_diff(1.0);
    let mut expected = [0u8; 32];
    expected[26] = 0xFF;
    expected[27] = 0xFF;
    assert_eq!(target, expected);
}

#[test]
fn target_from_diff_one_over_65536_shifts_up() {
    // Diff 1/65536: target scaled up by 2^16, so the 0xFFFF pair moves
    // from LE bytes 26..28 to 28..30.
    let target = target_from_diff(1.0 / 65536.0);
    let mut expected = [0u8; 32];
    expected[28] = 0xFF;
    expected[29] = 0xFF;
    assert_eq!(target, expected);
}

#[test]
fn hash_meets_target_accepts_block_1_at_diff_one() {
    // Block 1 hash has share diff ~1.99, so it should satisfy diff 1.
    let version: u32 = 0x0000_0001;
    let prev_hash = hex32("0a8ce26f72b3f1b646a2a6c14ff763ae65831e939c085ae10019d66800000000");
    let merkle_root = hex32("982051fd1e4ba744bbbe680e1fee14677ba1a3c3540bf7b1cdb606e857233e0e");
    let ntime: u32 = 0x4966_BC61;
    let nbits: u32 = 0x1D00_FFFF;
    let nonce: u32 = 0x9962_E301;

    let hash = header_hash(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let target = target_from_diff(1.0);
    assert!(hash_meets_target(&hash, &target));

    // At diff 2.0 the same hash should NOT meet the target (share diff < 2).
    let target = target_from_diff(2.0);
    assert!(!hash_meets_target(&hash, &target));
}

#[test]
fn header_hash_reports_redacted_diff() {
    // The reference-miner test asserts the share's difficulty truncates to 18.
    // diff = TRUE_DIFF_ONE / le256(hash). A correct header therefore
    // produces a hash below the diff-18 threshold.
    let version: u32 = 0x2000_0004;
    let prev_hash = hex32("d02b10fc0d4711eae1a805af50a8a83312a2215e00017f2b0000000000000000");
    let merkle_root = hex32("6d0359c451434605c52a5a9ce074340be47c2c63840731f9edf1db3f26b1cdd9");
    let ntime: u32 = 0x646F_F1A9;
    let nbits: u32 = 0x1705_AE3A;
    let nonce: u32 = 0x276E_8947;

    let hash = header_hash(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let diff = TRUE_DIFF_ONE / le256_to_f64(&hash);

    // reference-miner truncates diff to int and checks == 18. We allow a
    // small window because the f64 path may cross the bucket boundary.
    assert!(
        (17.0..19.0).contains(&diff),
        "expected diff ~18, got {diff} for hash {hash:?}",
    );
}
