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
    assert_eq!(
        rolled_version(0x2000_0000, 0, BIP320_VERSION_MASK),
        0x2000_0000
    );
    assert_eq!(
        rolled_version(0x2000_0000, 0x0027_0000, BIP320_VERSION_MASK),
        0x2027_0000
    );
    // Out-of-field bits in the rolled value are masked away.
    assert_eq!(
        rolled_version(0x2000_0000, !0, BIP320_VERSION_MASK),
        0x3FFF_E000
    );
}

/// A partial rolled mask merges only its own bits; a fixed bit outside
/// the rolled mask survives from the base version regardless of what
/// the echoed value carries there.
#[test]
fn rolled_version_merges_only_the_rolled_mask() {
    let base = 0x2004_0000;
    let rolled_mask = 0x0000_E000;
    let echoed = 0x1FFF_E000;
    assert_eq!(rolled_version(base, echoed, rolled_mask), 0x2004_E000);
}

/// A single chain gets the whole granted mask to roll and no fixed
/// bits.
#[test]
fn partition_version_mask_single_chain_keeps_whole_mask() {
    let partition = partition_version_mask(BIP320_VERSION_MASK, HashboardIndex(0), 1);
    assert_eq!(partition.fixed_bits, 0);
    assert_eq!(partition.rolled_mask, BIP320_VERSION_MASK);
}

/// Three chains share one rolled mask (the granted mask minus its top
/// two set bits, since `index_bits` for 3 chains is 2) and differ only
/// in which bits are fixed. The fixed bits are pairwise distinct and
/// never overlap the shared rolled mask.
#[test]
fn partition_version_mask_three_chains_share_rolled_mask_and_differ_in_fixed_bits() {
    let granted = BIP320_VERSION_MASK;
    let chains = 3;
    let partitions: heapless::Vec<_, 3> = (0..3)
        .map(|chain| partition_version_mask(granted, HashboardIndex(chain), chains))
        .collect();

    let rolled_mask = partitions[0].rolled_mask;
    for partition in &partitions {
        assert_eq!(partition.rolled_mask, rolled_mask);
        assert_eq!(partition.fixed_bits & rolled_mask, 0);
    }
    assert_eq!(partitions[0].fixed_bits, 0);
    assert_ne!(partitions[0].fixed_bits, partitions[1].fixed_bits);
    assert_ne!(partitions[0].fixed_bits, partitions[2].fixed_bits);
    assert_ne!(partitions[1].fixed_bits, partitions[2].fixed_bits);
}

/// An empty granted mask stays empty for every chain: nothing to fix
/// and nothing to roll.
#[test]
fn partition_version_mask_empty_mask_stays_empty() {
    for chain in 0..3 {
        let partition = partition_version_mask(0, HashboardIndex(chain), 3);
        assert_eq!(partition.fixed_bits, 0);
        assert_eq!(partition.rolled_mask, 0);
    }
}

/// A one-bit mask cannot supply the two index bits three chains need:
/// chain 0 keeps the whole (one-bit) mask to roll, and every other
/// chain gets nothing, pinning its version to the base value.
#[test]
fn partition_version_mask_too_few_bits_pins_every_chain_but_the_first() {
    let granted = 0x0000_2000;
    assert_eq!(
        partition_version_mask(granted, HashboardIndex(0), 3),
        VersionPartition {
            fixed_bits: 0,
            rolled_mask: granted,
        }
    );
    for chain in 1..3 {
        assert_eq!(
            partition_version_mask(granted, HashboardIndex(chain), 3),
            VersionPartition {
                fixed_bits: 0,
                rolled_mask: 0,
            }
        );
    }
}

/// One echo style: the chip returns only the bits it rolled, so
/// `version_bits` carries just the rolled index and the fixed bits must
/// already sit in the base version the consumer built at dispatch.
#[test]
fn rolled_version_reconstructs_a_rolled_bits_only_echo_for_a_partitioned_chain() {
    let granted = BIP320_VERSION_MASK;
    let base = 0x2000_0000;
    let chain = HashboardIndex(1);
    let chains = 3;
    let partition = partition_version_mask(granted, chain, chains);

    // What WorkJob::version carries: the base with this chain's fixed
    // bits already folded in.
    let base_with_fixed = (base & !granted) | partition.fixed_bits;

    // The chip rolls some index within its rolled_mask and echoes only
    // that.
    let rolled_index = 0x0000_4000 & partition.rolled_mask;
    let echoed = rolled_index;

    let header_version = rolled_version(base_with_fixed, echoed, partition.rolled_mask);

    // Reconstructing the header the chip actually hashed: base outside
    // the grant, this chain's fixed bits, and the rolled index.
    let expected = (base & !granted) | partition.fixed_bits | rolled_index;
    assert_eq!(header_version, expected);
}

/// Whole-field echo: the chip returns the whole field value, fixed
/// bits included, but the rolled mask still narrows the merge to the
/// bits that can legitimately differ, so the result is identical to the
/// reconstruction the rolled-only echo above produces.
#[test]
fn rolled_version_reconstructs_a_whole_field_echo_for_a_partitioned_chain() {
    let granted = BIP320_VERSION_MASK;
    let base = 0x2000_0000;
    let chain = HashboardIndex(1);
    let chains = 3;
    let partition = partition_version_mask(granted, chain, chains);

    let base_with_fixed = (base & !granted) | partition.fixed_bits;

    let rolled_index = 0x0000_4000 & partition.rolled_mask;
    // The whole field value, fixed bits included.
    let echoed = partition.fixed_bits | rolled_index;

    let header_version = rolled_version(base_with_fixed, echoed, partition.rolled_mask);

    let expected = (base & !granted) | partition.fixed_bits | rolled_index;
    assert_eq!(header_version, expected);
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
