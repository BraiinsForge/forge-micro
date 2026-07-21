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

#[test]
fn decode_round_trips_known_bytes() {
    let mut out = [0u8; 4];
    assert!(decode_to_slice(b"deadbeef", &mut out));
    assert_eq!(out, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn decode_accepts_uppercase() {
    let mut out = [0u8; 4];
    assert!(decode_to_slice(b"DEADBEEF", &mut out));
    assert_eq!(out, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn decode_accepts_mixed_case() {
    let mut out = [0u8; 4];
    assert!(decode_to_slice(b"DeAdBeEf", &mut out));
    assert_eq!(out, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn decode_rejects_wrong_length() {
    let mut out = [0u8; 4];
    assert!(!decode_to_slice(b"deadbe", &mut out));
    assert!(!decode_to_slice(b"deadbeef00", &mut out));
}

#[test]
fn decode_rejects_invalid_nibble() {
    let mut out = [0u8; 4];
    assert!(!decode_to_slice(b"deadbeeg", &mut out));
    assert!(!decode_to_slice(b"!eadbeef", &mut out));
}

#[test]
fn encode_lower_round_trips_known_bytes() {
    let mut out = [0u8; 8];
    assert!(encode_lower_to_slice(&[0xde, 0xad, 0xbe, 0xef], &mut out));
    assert_eq!(&out, b"deadbeef");
}

#[test]
fn encode_lower_is_always_lowercase() {
    let mut out = [0u8; 32];
    assert!(encode_lower_to_slice(&[0xff; 16], &mut out));
    assert_eq!(&out, b"ffffffffffffffffffffffffffffffff");
}

#[test]
fn encode_lower_rejects_wrong_length() {
    let mut out = [0u8; 7];
    assert!(!encode_lower_to_slice(&[0xde, 0xad, 0xbe, 0xef], &mut out));
}

#[test]
fn encode_block_hash_lower_reverses_to_msb_first() {
    // Wire fixture: LSB-first bytes [01, 23, 45, 67, 89, ab, cd, ef]
    // in positions 0..8, then 24 zero bytes. MSB-first display
    // reverses the whole array, so the zero bytes (high indices)
    // print first and the pattern (low indices) prints at the tail
    // in reverse order: "...efcdab8967452301".
    let mut wire = [0u8; 32];
    wire[..8].copy_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
    let encoded = encode_block_hash_lower(&wire);
    assert_eq!(&encoded[..48], &[b'0'; 48]);
    assert_eq!(&encoded[48..], b"efcdab8967452301");
}

#[test]
fn encode_block_hash_lower_all_zero() {
    let encoded = encode_block_hash_lower(&[0u8; 32]);
    assert_eq!(
        &encoded,
        b"0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[test]
fn encode_block_hash_lower_all_ff() {
    let encoded = encode_block_hash_lower(&[0xffu8; 32]);
    assert_eq!(
        &encoded,
        b"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
    );
}

#[test]
fn round_trip_encode_then_decode_lowercase() {
    let original = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
    ];
    let mut encoded = [0u8; 24];
    assert!(encode_lower_to_slice(&original, &mut encoded));
    let mut decoded = [0u8; 12];
    assert!(decode_to_slice(&encoded, &mut decoded));
    assert_eq!(decoded, original);
}
