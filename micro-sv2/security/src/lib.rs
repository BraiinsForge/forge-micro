#![no_std]

//! User-friendly Stratum V2 security types.
//!
//! Wraps [`ii_sv2_crypto`] with the SV2-spec key encodings (base58check
//! with the `priv` / version-1 prefixes), certificate types, and an
//! async, runtime-agnostic handshake driver. `no_std`, no-alloc.

pub mod error;
pub mod keys;

pub use error::KeyParseError;
pub use keys::{PublicKey, SecretKey, SessionId, Signature};
