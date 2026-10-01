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

//! The [`Storage`] trait and the blob framing: an 8-byte schema hash
//! ahead of the postcard payload, so schema drift across firmware
//! updates is detected without a version field.

// Embassy executors do not require `Send`, and the bound would cost
// every storage impl.
#![allow(async_fn_in_trait)]

use serde::Serialize;

use crate::Config;

/// Map key of the one config record.
pub const CONFIG_KEY: u8 = 0;

/// Map key of the record of the seed fingerprints the store last took.
pub const SEED_KEY: u8 = 1;

#[derive(Debug)]
pub enum LoadError<E> {
    /// The prefix differs from [`Config::SCHEMA_HASH`]: the firmware was
    /// updated across a schema change.
    SchemaMismatch {
        stored: [u8; 8],
        expected: [u8; 8],
    },
    /// The payload does not deserialize, as after a partial write.
    Corrupt,
    Io(E),
}

/// Async key-value storage; the framing sits above it.
pub trait Storage {
    type Error;

    /// `Ok(None)` when `key` was never written. A value larger than
    /// `buffer` is truncated, which [`decode_blob`] then reports as
    /// [`LoadError::Corrupt`].
    async fn load<'b>(
        &mut self,
        key: u8,
        buffer: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, Self::Error>;

    async fn store(&mut self, key: u8, data: &[u8]) -> Result<(), Self::Error>;
}

/// Encode `config` behind its schema hash and return the length.
///
/// # Errors
///
/// When `buffer` is too small for the payload.
pub fn encode_blob(config: &Config, buffer: &mut [u8]) -> Result<usize, postcard::Error> {
    encode_record(Config::SCHEMA_HASH, config, buffer)
}

/// Encode `schema` into `buffer` behind `hash`, the schema hash of the
/// version whose record it is, and return the record length.
///
/// The live config and every older version frame their record the same
/// way, so one encoder serves them all.
pub(crate) fn encode_record<T: Serialize>(
    hash: [u8; 8],
    schema: &T,
    buffer: &mut [u8],
) -> Result<usize, postcard::Error> {
    const HASH_LEN: usize = 8;
    if buffer.len() < HASH_LEN {
        return Err(postcard::Error::SerializeBufferFull);
    }
    buffer[..HASH_LEN].copy_from_slice(&hash);
    let payload = postcard::to_slice(schema, &mut buffer[HASH_LEN..])?;
    Ok(HASH_LEN + payload.len())
}

/// Decode a blob. `E` is never produced here; it makes the return type
/// match the caller's I/O errors.
///
/// # Errors
///
/// [`LoadError::SchemaMismatch`] on a different prefix,
/// [`LoadError::Corrupt`] on a short blob or a malformed payload.
pub fn decode_blob<E>(blob: &[u8]) -> Result<Config, LoadError<E>> {
    const HASH_LEN: usize = 8;
    if blob.len() < HASH_LEN {
        return Err(LoadError::Corrupt);
    }
    let mut stored = [0u8; 8];
    stored.copy_from_slice(&blob[..HASH_LEN]);
    let expected = Config::SCHEMA_HASH;
    if stored != expected {
        return Err(LoadError::SchemaMismatch { stored, expected });
    }
    postcard::from_bytes(&blob[HASH_LEN..]).map_err(|_| LoadError::Corrupt)
}
