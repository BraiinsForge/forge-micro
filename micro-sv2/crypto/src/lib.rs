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

#![no_std]

//! Stratum V2 `Noise_NX_secp256k1_chachapoly_sha256` crypto core.
//!
//! `no_std`. The caller supplies a [`Rng`] and a [`CryptoCtx`] that
//! wraps a long-lived secp256k1 precomputation context. The default
//! `alloc` feature enables the heap-allocated [`CryptoCtx::new`]
//! constructor; `default-features = false` keeps the crate alloc-free
//! and falls back to [`CryptoCtx::preallocated`] over a static buffer.
//! See the crate README and `tests/handshake.rs` for an end-to-end
//! example.

mod aead;
pub mod ctx;
pub mod error;
pub mod handshake;
mod hash;
pub mod keys;
pub mod rng;
pub mod transport;

pub use ctx::CryptoCtx;
pub use error::{AeadError, NXError, Secp256k1KeyError};
pub use handshake::{
    EPHEMERAL_KEY_MSG_SIZE, EPHEMERAL_KEY_SIZE, HandShake, PROTOCOL_NAME, PROTOCOL_NAME_HASH,
    PROTOCOL_NAME_HASH_HASH, STATIC_KEY_MSG_SIZE, STATIC_KEY_SIZE,
};
pub use keys::{PublicSecp256k1Key, Secp256k1Elligator, Secp256k1KeyPair, SecretSecp256k1Key};
pub use rng::Rng;
pub use transport::TransportState;

/// AEAD authentication tag length (ChaCha20-Poly1305).
pub const MAC_SIZE: usize = 16;

/// secp256k1 secret key length.
pub const SECP256_SECRET_KEY_LENGTH: usize = 32;

/// Ellswift-encoded public key length (BIP-324).
pub const SECP256_ELLSWIFT_PUBLIC_KEY_LENGTH: usize = 64;

/// X-only secp256k1 public key length.
pub const SECP256_XONLY_PUBLIC_KEY_LENGTH: usize = 32;

/// ECDH shared secret length.
pub const ECDH_LENGTH: usize = 32;
