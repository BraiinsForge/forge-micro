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

//! Unit tests for the CPU-mock ASIC primitives.

use super::*;
use stratum_client::WorkAssignment;

/// Hash one nonce and report whether it meets the target.
///
/// Lets tests cover the inner loop without spinning the asynchronous
/// machinery.
#[must_use]
fn nonce_meets_target(work: &WorkAssignment, nonce: u32) -> bool {
    let header = build_header(work, nonce);
    let hash = sha256d(&header);
    hash_meets_target(&hash, &work.target)
}

fn fixture(target: [u8; 32]) -> WorkAssignment {
    WorkAssignment {
        work_id: 1,
        prev_hash: [0; 32],
        merkle_root: [0; 32],
        target,
        version: 0x2000_0000,
        version_mask: 0,
        nbits: 0x1d00_ffff,
        ntime: 0x6044_9522,
    }
}

#[test]
fn hash_meets_max_target_for_any_nonce() {
    // `target = 0xFF..FF` is the most permissive — every hash wins.
    let work = fixture([0xFFu8; 32]);
    assert!(nonce_meets_target(&work, 0));
    assert!(nonce_meets_target(&work, 0x1234_5678));
}

#[test]
fn hash_misses_zero_target() {
    // Zero target is unreachable.
    let work = fixture([0u8; 32]);
    assert!(!nonce_meets_target(&work, 0));
    assert!(!nonce_meets_target(&work, 1));
}

#[test]
fn build_header_layout_is_canonical() {
    let work = WorkAssignment {
        work_id: 0,
        prev_hash: [0xAA; 32],
        merkle_root: [0xBB; 32],
        target: [0; 32],
        version: 0x1234_5678,
        version_mask: 0,
        nbits: 0x9ABC_DEF0,
        ntime: 0x6112_3456,
    };
    let header = build_header(&work, 0xCAFE_BABE);

    // version little-endian
    assert_eq!(&header[0..4], &[0x78, 0x56, 0x34, 0x12]);
    // prev_hash bytes verbatim
    assert_eq!(&header[4..36], &[0xAA; 32]);
    // merkle_root bytes verbatim
    assert_eq!(&header[36..68], &[0xBB; 32]);
    // ntime little-endian
    assert_eq!(&header[68..72], &[0x56, 0x34, 0x12, 0x61]);
    // nbits little-endian
    assert_eq!(&header[72..76], &[0xF0, 0xDE, 0xBC, 0x9A]);
    // nonce little-endian
    assert_eq!(&header[76..80], &[0xBE, 0xBA, 0xFE, 0xCA]);
}
