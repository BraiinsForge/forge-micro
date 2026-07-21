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

//! Tests for `const_fnv1a` hash stability and `encode_args` round-trip
//! correctness across all six argument types.

use crate::{Arg, encode_args};

#[test]
fn fnv1a_deterministic() {
    assert_eq!(
        crate::const_fnv1a("thermal_limit"),
        crate::const_fnv1a("thermal_limit")
    );
    assert_eq!(crate::const_fnv1a(""), crate::const_fnv1a(""));
}

#[test]
fn fnv1a_distinct() {
    assert_ne!(
        crate::const_fnv1a("thermal_limit"),
        crate::const_fnv1a("sensor_fault")
    );
    assert_ne!(crate::const_fnv1a("foo"), crate::const_fnv1a("bar"));
}

fn decode_u8(payload: &[u8; 11], offset: usize) -> u8 {
    payload[offset]
}

fn decode_u16(payload: &[u8; 11], offset: usize) -> u16 {
    u16::from_le_bytes([payload[offset], payload[offset + 1]])
}

fn decode_u32(payload: &[u8; 11], offset: usize) -> u32 {
    u32::from_le_bytes([
        payload[offset],
        payload[offset + 1],
        payload[offset + 2],
        payload[offset + 3],
    ])
}

fn decode_i8(payload: &[u8; 11], offset: usize) -> i8 {
    i8::from_le_bytes([payload[offset]])
}

fn decode_i16(payload: &[u8; 11], offset: usize) -> i16 {
    i16::from_le_bytes([payload[offset], payload[offset + 1]])
}

fn decode_i32(payload: &[u8; 11], offset: usize) -> i32 {
    i32::from_le_bytes([
        payload[offset],
        payload[offset + 1],
        payload[offset + 2],
        payload[offset + 3],
    ])
}

#[test]
fn encode_u8_round_trip() {
    let payload = encode_args(&[Arg::U8(42)]);
    assert_eq!(decode_u8(&payload, 0), 42u8);
}

#[test]
fn encode_u16_round_trip() {
    let payload = encode_args(&[Arg::U16(1234)]);
    assert_eq!(decode_u16(&payload, 0), 1234u16);
}

#[test]
fn encode_u32_round_trip() {
    let payload = encode_args(&[Arg::U32(0xDEAD_BEEF)]);
    assert_eq!(decode_u32(&payload, 0), 0xDEAD_BEEFu32);
}

#[test]
fn encode_i8_round_trip() {
    let payload = encode_args(&[Arg::I8(-7)]);
    assert_eq!(decode_i8(&payload, 0), -7i8);
}

#[test]
fn encode_i16_round_trip() {
    let payload = encode_args(&[Arg::I16(-1000)]);
    assert_eq!(decode_i16(&payload, 0), -1000i16);
}

#[test]
fn encode_i32_round_trip() {
    let payload = encode_args(&[Arg::I32(-100_000)]);
    assert_eq!(decode_i32(&payload, 0), -100_000i32);
}

#[test]
fn encode_mixed_three_args_round_trip() {
    // u16(500) + i8(-3) + u32(0x1234_5678): 2 + 1 + 4 = 7 bytes, fits.
    let payload = encode_args(&[Arg::U16(500), Arg::I8(-3), Arg::U32(0x1234_5678)]);
    assert_eq!(decode_u16(&payload, 0), 500u16);
    assert_eq!(decode_i8(&payload, 2), -3i8);
    assert_eq!(decode_u32(&payload, 3), 0x1234_5678u32);
}
