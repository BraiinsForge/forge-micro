#![no_std]

//! Stratum V2 `Noise_NX_secp256k1_chachapoly_sha256` crypto core.
//!
//! `no_std`, no-alloc. The caller supplies a [`secp256k1::Secp256k1`]
//! context and a [`KeyGen`] random source. See the crate README and
//! `tests/handshake.rs` for an end-to-end example.

mod aead;
pub mod error;
pub mod handshake;
mod hash;
pub mod keygen;
pub mod keys;
pub mod transport;

pub use error::{AeadError, NXError, Secp256k1KeyError};
pub use handshake::{
    EPHEMERAL_KEY_MSG_SIZE, EPHEMERAL_KEY_SIZE, HandShake, PROTOCOL_NAME, PROTOCOL_NAME_HASH,
    PROTOCOL_NAME_HASH_HASH, STATIC_KEY_MSG_SIZE, STATIC_KEY_SIZE,
};
pub use keygen::KeyGen;
pub use keys::{PublicSecp256k1Key, Secp256k1Elligator, Secp256k1KeyPair, SecretSecp256k1Key};
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
