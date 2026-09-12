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

use super::*;

/// Reverse the bytes within each 4-byte word of a 256-bit value.
///
/// Stratum's `mining.notify` emits `prev_hash` as eight big-endian 32-bit
/// words, but Bitcoin's block header wants them in native (little-endian)
/// byte order inside each word. `mining_protocol::hashing::build_header`
/// applies this swap inline; ubos-mining stores `WorkJob.prev_hash` in
/// canonical post-swap form so both the local hash check and the chip
/// wire frame see the same bytes.
fn word_swap_prev_hash(input: &[u8; 32]) -> [u8; 32] {
    let mut output = [0u8; 32];
    for word_index in 0..8 {
        let offset = word_index * 4;
        output[offset] = input[offset + 3];
        output[offset + 1] = input[offset + 2];
        output[offset + 2] = input[offset + 1];
        output[offset + 3] = input[offset];
    }
    output
}

/// Locks ubos-mining's local share-verification hash to
/// `mining_protocol::hashing::header_hash`. Both sides reconstruct the
/// same canonical block header from the same `WorkAssignment` fields,
/// and the pool will reject any submit whose chip-side hash diverges
/// from what it recomputes for the same `(prev_hash, ntime, nonce, ...)`.
#[test]
fn solution_hash_matches_mining_protocol_reference() {
    let mut stratum_prev = [0u8; 32];
    for (index, byte) in stratum_prev.iter_mut().enumerate() {
        // `enumerate` over a 32-byte array yields 0..32.
        *byte = u8::try_from(index).expect("BUG: index < 32");
    }
    let mut merkle = [0u8; 32];
    for (index, byte) in merkle.iter_mut().enumerate() {
        // `enumerate` over a 32-byte array yields 0..32.
        *byte = u8::try_from(32 - index).expect("BUG: 32 - index in 1..=32");
    }
    let version: u32 = 0x2000_0000;
    let ntime: u32 = 0x6700_0000;
    let nbits: u32 = 0x1d00_ffff;
    let nonce: u32 = 0xdead_beef;

    // The stratum adapter word-swaps prev_hash on arrival, so the
    // consumer hashes the canonical internal byte order. Simulate that
    // here so the reference header_hash (which swaps inline) produces
    // the same 80-byte header.
    let actual_hash = solution_hash(
        version,
        &word_swap_prev_hash(&stratum_prev),
        &merkle,
        ntime,
        nbits,
        nonce,
    );
    let reference =
        mining_protocol::hashing::header_hash(version, stratum_prev, merkle, ntime, nbits, nonce);
    assert_eq!(actual_hash, reference);
}

/// `rolled_version` replaces only the BIP320 field, keeping every
/// out-of-field bit of the base version.
#[test]
fn rolled_version_replaces_only_the_bip320_field() {
    assert_eq!(rolled_version(0x2000_0000, 0), 0x2000_0000);
    assert_eq!(rolled_version(0x2000_0000, 0x0027_0000), 0x2027_0000);
    // Out-of-field bits in the rolled value are masked away.
    assert_eq!(rolled_version(0x2000_0000, !0), 0x3FFF_E000);
}

/// `from_pow2_difficulty` reproduces the hand-built byte arrays it
/// replaced: difficulty one places `0xFFFF` at bit 208, and each
/// doubling of the difficulty shifts the mantissa right one bit.
#[test]
fn from_pow2_difficulty_places_the_mantissa() {
    let mut diff_one = [0u8; 32];
    diff_one[26] = 0xFF;
    diff_one[27] = 0xFF;
    assert_eq!(*ShareTarget::DIFF_1, diff_one);

    let mut diff_eight = [0u8; 32];
    diff_eight[25] = 0xE0;
    diff_eight[26] = 0xFF;
    diff_eight[27] = 0x1F;
    assert_eq!(*ShareTarget::from_pow2_difficulty(8), diff_eight);
    assert_eq!(
        ShareTarget::from_pow2_difficulty(8).to_le_bytes(),
        diff_eight
    );
}

#[test]
fn share_difficulty_obeys_chain_and_chip_record_limits() {
    let period = embassy_time::Duration::from_millis(480);

    assert_eq!(
        ShareDifficulty::for_chain(1_216, 12, Frequency::from_mhz(210), period, 128, 1),
        ShareDifficulty::from_pow2(32)
    );
    assert_eq!(
        ShareDifficulty::for_chain(1_216, 160, Frequency::from_mhz(660), period, 128, 1),
        ShareDifficulty::from_pow2(256)
    );
    assert_eq!(
        ShareDifficulty::for_chain(1_216, 0, Frequency::from_mhz(1), period, 128, 1),
        ShareDifficulty::ONE
    );
}
