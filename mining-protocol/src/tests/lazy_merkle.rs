//! Tests for `LazyMerkle::compute`.

use super::*;

use sha2::{Digest, Sha256};

fn sha256d(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

#[test]
fn final_variant_is_a_memcpy_and_ignores_extranonce_2() {
    let pattern = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
        0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    ];
    let merkle = LazyMerkle::Final(&pattern);

    let mut root = [0u8; 32];
    merkle.compute(&[], &mut root);
    assert_eq!(root, pattern);

    // Non-empty extranonce_2 must not influence the result.
    let mut root_with_en2 = [0u8; 32];
    merkle.compute(b"\xff\xee\xdd\xcc", &mut root_with_en2);
    assert_eq!(root_with_en2, pattern);
}

#[test]
fn prepared_variant_no_branches_matches_direct_sha256d() {
    let coinbase_prefix = b"\x01\x02\x03\x04";
    let extranonce_prefix = b"\xaa\xbb";
    let extranonce_2 = b"\x10\x20\x30\x40";
    let coinbase_suffix = b"\x05\x06\x07\x08\x09\x0a";

    let mut concat = [0u8; 16];
    concat[..4].copy_from_slice(coinbase_prefix);
    concat[4..6].copy_from_slice(extranonce_prefix);
    concat[6..10].copy_from_slice(extranonce_2);
    concat[10..16].copy_from_slice(coinbase_suffix);
    let expected = sha256d(&concat);

    let merkle = LazyMerkle::Prepared {
        extranonce_prefix,
        coinbase_prefix,
        coinbase_suffix,
        branches: &[],
    };
    let mut root = [0u8; 32];
    merkle.compute(extranonce_2, &mut root);

    assert_eq!(root, expected);
}

#[test]
fn prepared_variant_one_branch_matches_two_step_sha256d() {
    let coinbase_prefix = b"";
    let extranonce_prefix = b"";
    let extranonce_2 = b"\x00\x00\x00\x00";
    let coinbase_suffix = b"\xff";

    let leaf = sha256d(b"\x00\x00\x00\x00\xff");

    let branch: [u8; 32] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
    ];

    let mut concat = [0u8; 64];
    concat[..32].copy_from_slice(&leaf);
    concat[32..].copy_from_slice(&branch);
    let expected = sha256d(&concat);

    let branches = [branch];
    let merkle = LazyMerkle::Prepared {
        extranonce_prefix,
        coinbase_prefix,
        coinbase_suffix,
        branches: &branches,
    };
    let mut root = [0u8; 32];
    merkle.compute(extranonce_2, &mut root);

    assert_eq!(root, expected);
}

#[test]
fn prepared_variant_extranonce_2_changes_root() {
    let coinbase_prefix = b"prefix";
    let extranonce_prefix = b"en1\x00";
    let coinbase_suffix = b"suffix";
    let branches = [[0u8; 32]; 0];

    let merkle = LazyMerkle::Prepared {
        extranonce_prefix,
        coinbase_prefix,
        coinbase_suffix,
        branches: &branches,
    };

    let mut root_a = [0u8; 32];
    merkle.compute(b"\x00\x00\x00\x00", &mut root_a);

    let mut root_b = [0u8; 32];
    merkle.compute(b"\x00\x00\x00\x01", &mut root_b);

    assert_ne!(root_a, root_b, "rolling extranonce_2 must change the root");
}
