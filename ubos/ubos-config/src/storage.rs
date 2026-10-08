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

//! Storage abstraction for persisting [`Config`] to non-volatile memory.
//!
//! Defines the async [`Storage`] trait, the typed [`LoadError`], and the
//! two free functions [`encode_blob`] / [`decode_blob`] that frame
//! serialized configs with an 8-byte schema-hash prefix. The prefix
//! lets the firmware detect schema drift across firmware updates without
//! a separate manual version field.

// Embassy executors do not require `Send`; adding the bound to every
// trait method's future would force every storage impl and all internal
// state to be `Send`, which is not free on no_std embedded targets.
#![allow(async_fn_in_trait)]

use crate::Config;

/// Map key for the config record.
///
/// A `u8` key is sufficient for the store's few record types, and each
/// type has a constant of its own.
pub const CONFIG_KEY: u8 = 0;

/// Map key for the record of the seed fingerprints the store last took.
pub const SEED_KEY: u8 = 1;

/// Reason a stored blob could not be decoded back into a [`Config`].
#[derive(Debug)]
pub enum LoadError<E> {
    /// The stored blob's 8-byte prefix does not match
    /// [`Config::SCHEMA_HASH`]. Firmware was probably updated and the
    /// on-flash layout changed; the caller should fall back to defaults.
    SchemaMismatch {
        /// Hash found at the start of the stored blob.
        stored: [u8; 8],
        /// Hash compiled into the running firmware.
        expected: [u8; 8],
    },
    /// The postcard payload could not be deserialized. The data may
    /// have been partially written before a power failure.
    Corrupt,
    /// An I/O error from the underlying storage backend.
    Io(E),
}

/// Async key-value storage backend.
///
/// The key type is `u8` matching [`CONFIG_KEY`] and [`SEED_KEY`].
/// Values are raw byte slices; framing (schema hash, serialization) is
/// handled by [`encode_blob`] and [`decode_blob`] above this layer.
pub trait Storage {
    /// Backend-specific error type.
    type Error;

    /// Read the value for `key` into `buffer`.
    ///
    /// Returns `Ok(Some(slice))` when the key is present, `Ok(None)`
    /// when it has never been written, and `Err` on I/O failure. The
    /// returned slice is a sub-slice of `buffer`.
    ///
    /// Callers must size `buffer` to fit the maximum expected blob.
    /// If a stored value is larger than `buffer`, it is silently
    /// truncated; [`decode_blob`] then surfaces the truncation as
    /// [`LoadError::Corrupt`]. Today's callers pass `LOAD_BUFFER_SIZE`,
    /// which fits the maximum [`Config`] blob.
    async fn load<'b>(
        &mut self,
        key: u8,
        buffer: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, Self::Error>;

    /// Write `data` under `key`, replacing any previous value.
    async fn store(&mut self, key: u8, data: &[u8]) -> Result<(), Self::Error>;
}

/// Encode `config` into `buffer` with the schema-hash prefix.
///
/// The first 8 bytes of the output are [`Config::SCHEMA_HASH`]. The
/// remaining bytes are the postcard-serialized `config`. Returns the
/// total number of bytes written, which is the slice length the caller
/// should pass to [`Storage::store`].
///
/// # Errors
///
/// Returns `postcard::Error` when `buffer` is too small to hold the
/// serialized config (the schema-hash prefix always fits in any
/// buffer ≥ 8 bytes, so the error is from the postcard encoding step).
pub fn encode_blob(config: &Config, buffer: &mut [u8]) -> Result<usize, postcard::Error> {
    const HASH_LEN: usize = 8;
    if buffer.len() < HASH_LEN {
        return Err(postcard::Error::SerializeBufferFull);
    }
    buffer[..HASH_LEN].copy_from_slice(&Config::SCHEMA_HASH);
    let payload = postcard::to_slice(config, &mut buffer[HASH_LEN..])?;
    Ok(HASH_LEN + payload.len())
}

/// Decode a schema-prefixed blob back into a [`Config`].
///
/// Checks that the blob is at least 8 bytes long, compares the prefix
/// against [`Config::SCHEMA_HASH`], then postcard-deserializes the
/// remainder. The type parameter `E` is the backend error type; this
/// function never produces `LoadError::Io(E)`, but the type must be
/// supplied by the caller so the return type is compatible with
/// `LoadError` values that may include I/O errors from the same site.
///
/// # Errors
///
/// - [`LoadError::SchemaMismatch`] when the prefix does not match.
/// - [`LoadError::Corrupt`] when the postcard payload is malformed or
///   the blob is shorter than 8 bytes.
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
