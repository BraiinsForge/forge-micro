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

//! `no_std` lowercase-hex encode/decode plus the conventional
//! 32-byte block-hash display helper.
//!
//! Lives here because every adapter (`stratum-v1` wire decode, the
//! V1 and V2 `ubos_logging` call sites) and a couple of integration
//! crates need the same thing. Centralising means one set of tests
//! and no drift between hand-rolled copies.

#[cfg(test)]
#[path = "tests/hex.rs"]
mod tests;

/// Decode hex `input` into `output`, byte-for-byte. Accepts both
/// upper- and lower-case digits.
///
/// Returns `false` if `input.len() != output.len() * 2` or if any
/// byte of `input` is not an ASCII hex digit. On `false`,
/// `output` may have been partially overwritten — callers reading
/// the buffer after a failure must treat its contents as
/// undefined.
#[must_use]
pub fn decode_to_slice(input: &[u8], output: &mut [u8]) -> bool {
    if input.len() != output.len() * 2 {
        return false;
    }
    for index in 0..output.len() {
        let Some(high) = decode_nibble(input[index * 2]) else {
            return false;
        };
        let Some(low) = decode_nibble(input[index * 2 + 1]) else {
            return false;
        };
        output[index] = (high << 4) | low;
    }
    true
}

/// Encode `input` into lowercase hex in `output`, byte-for-byte.
///
/// Returns `false` if `output.len() != input.len() * 2`. Never
/// produces uppercase digits.
#[must_use]
pub fn encode_lower_to_slice(input: &[u8], output: &mut [u8]) -> bool {
    if output.len() != input.len() * 2 {
        return false;
    }
    for (index, byte) in input.iter().enumerate() {
        output[index * 2] = HEX_LOWER[(byte >> 4) as usize];
        output[index * 2 + 1] = HEX_LOWER[(byte & 0x0f) as usize];
    }
    true
}

/// Reverse a 32-byte little-endian hash or target into the
/// MSB-first byte order block explorers and bitcoin RPC use, then
/// encode as 64 lowercase hex digits. Suitable for `ubos_logging::info!`
/// via `{}` after `core::str::from_utf8(&...).expect(...)`.
#[must_use]
pub fn encode_block_hash_lower(input: &[u8; 32]) -> [u8; 64] {
    let mut output = [0u8; 64];
    for (index, byte) in input.iter().rev().enumerate() {
        output[index * 2] = HEX_LOWER[(byte >> 4) as usize];
        output[index * 2 + 1] = HEX_LOWER[(byte & 0x0f) as usize];
    }
    output
}

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

/// Value of one hex digit, or `None` when `byte` is not one.
///
/// Both letter cases are accepted on the way in, while
/// [`encode_lower_to_slice`] only ever emits the lower one. Pools differ
/// on which case they send and none of them cares which it receives.
fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
