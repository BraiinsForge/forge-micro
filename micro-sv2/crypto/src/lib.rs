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
pub use keys::{
    PublicSecp256k1Key, Secp256k1Elligator, Secp256k1KeyPair, SecretSecp256k1Key, new_keypair,
    new_private_key,
};
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
