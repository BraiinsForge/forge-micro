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

//! SV2-spec base58check codec for the pool authority public key.
//!
//! The wire form is `base58( prefix ‖ key ‖ checksum )` where the
//! prefix is [`SV2_PUBKEY_PREFIX`], `key` is the raw 32-byte xonly
//! public key, and `checksum` is the first four bytes of the double
//! SHA-256 of `prefix ‖ key` (Bitcoin-style `Base58Check`). Alloc-free:
//! base58 goes through fixed stack buffers so the firmware needs no
//! allocator.

use sha2::{Digest, Sha256};

/// Two-byte version prefix that precedes the 32-byte key inside the
/// SV2 base58check encoding.
pub const SV2_PUBKEY_PREFIX: [u8; 2] = [1, 0];

/// `prefix (2) + key (32) + checksum (4)`.
const PAYLOAD_LEN: usize = 38;

/// Reasons [`decode`] rejects an input string.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PubkeyError {
    /// Not a valid base58 string.
    BadBase58,
    /// Decoded payload is not [`PAYLOAD_LEN`] bytes.
    BadLength,
    /// Version prefix is not [`SV2_PUBKEY_PREFIX`].
    BadPrefix,
    /// Recomputed checksum does not match the trailing four bytes.
    BadChecksum,
}

impl core::fmt::Display for PubkeyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            PubkeyError::BadBase58 => "pubkey: not valid base58",
            PubkeyError::BadLength => "pubkey: wrong decoded length",
            PubkeyError::BadPrefix => "pubkey: wrong version prefix",
            PubkeyError::BadChecksum => "pubkey: checksum mismatch",
        })
    }
}

/// First four bytes of the double SHA-256 of `payload`.
fn checksum(payload: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(payload);
    let second = Sha256::digest(first);
    [second[0], second[1], second[2], second[3]]
}

/// Encode a raw 32-byte key as its SV2 base58check string.
#[must_use]
pub fn encode(key: &[u8; 32]) -> heapless::String<64> {
    let mut payload = [0u8; PAYLOAD_LEN];
    payload[..2].copy_from_slice(&SV2_PUBKEY_PREFIX);
    payload[2..34].copy_from_slice(key);
    let check = checksum(&payload[..34]);
    payload[34..].copy_from_slice(&check);

    let mut out = [0u8; 64];
    let len = bs58::encode(&payload[..])
        .onto(&mut out[..])
        .expect("BUG: 64 bytes hold the base58 of 38 bytes");
    let text = core::str::from_utf8(&out[..len]).expect("BUG: base58 is ASCII");
    heapless::String::try_from(text).expect("BUG: base58 of 38 bytes fits String<64>")
}

/// Decode an SV2 base58check string into the raw 32-byte key.
///
/// # Errors
///
/// Returns [`PubkeyError`] when the input is not base58, decodes to the
/// wrong length, carries the wrong prefix, or fails the checksum.
pub fn decode(encoded: &str) -> Result<[u8; 32], PubkeyError> {
    let mut buffer = [0u8; 40];
    let len = bs58::decode(encoded)
        .onto(&mut buffer[..])
        .map_err(|_| PubkeyError::BadBase58)?;
    if len != PAYLOAD_LEN {
        return Err(PubkeyError::BadLength);
    }
    if buffer[..2] != SV2_PUBKEY_PREFIX {
        return Err(PubkeyError::BadPrefix);
    }
    if checksum(&buffer[..34]) != buffer[34..38] {
        return Err(PubkeyError::BadChecksum);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&buffer[2..34]);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::DEFAULT_POOL_SV2_PUBKEY;

    /// The factory-default authority key in SV2 base58check form.
    const LITERAL: &str = "9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna";

    #[test]
    fn decode_default_literal_matches_bytes() {
        assert_eq!(decode(LITERAL), Ok(DEFAULT_POOL_SV2_PUBKEY));
    }

    #[test]
    fn encode_default_bytes_matches_literal() {
        assert_eq!(encode(&DEFAULT_POOL_SV2_PUBKEY).as_str(), LITERAL);
    }

    #[test]
    fn round_trip_arbitrary_key() {
        let key = [0x11u8; 32];
        assert_eq!(decode(encode(&key).as_str()), Ok(key));
    }

    #[test]
    fn rejects_non_base58() {
        // '0' is not in the base58 alphabet.
        assert_eq!(decode("0OIl"), Err(PubkeyError::BadBase58));
    }

    #[test]
    fn rejects_wrong_length() {
        // Valid base58, decodes to 3 bytes, not 38.
        assert_eq!(decode("abc"), Err(PubkeyError::BadLength));
    }

    #[test]
    fn rejects_wrong_prefix() {
        let mut payload = [0u8; PAYLOAD_LEN];
        payload[..2].copy_from_slice(&[9, 9]);
        payload[2..34].copy_from_slice(&[0x22u8; 32]);
        let check = checksum(&payload[..34]);
        payload[34..].copy_from_slice(&check);
        let mut out = [0u8; 64];
        let len = bs58::encode(&payload[..])
            .onto(&mut out[..])
            .expect("test payload fits the buffer");
        let text = core::str::from_utf8(&out[..len]).expect("base58 is ASCII");
        assert_eq!(decode(text), Err(PubkeyError::BadPrefix));
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut payload = [0u8; PAYLOAD_LEN];
        payload[..2].copy_from_slice(&SV2_PUBKEY_PREFIX);
        payload[2..34].copy_from_slice(&[0x33u8; 32]);
        // Correct checksum for a *different* payload → mismatch.
        payload[34..].copy_from_slice(&[0, 0, 0, 0]);
        let mut out = [0u8; 64];
        let len = bs58::encode(&payload[..])
            .onto(&mut out[..])
            .expect("test payload fits the buffer");
        let text = core::str::from_utf8(&out[..len]).expect("base58 is ASCII");
        assert_eq!(decode(text), Err(PubkeyError::BadChecksum));
    }
}
