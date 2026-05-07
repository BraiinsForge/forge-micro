#![no_std]

//! User-friendly Stratum V2 security types.
//!
//! Wraps [`ii_sv2_crypto`] with the SV2-spec key encodings (base58check
//! with the `priv` / version-1 prefixes), certificate types, and an
//! async, runtime-agnostic handshake driver. `no_std`, no-alloc.

pub mod authenticator;
pub mod certificate;
pub mod error;
pub mod handshake;
pub mod keys;
pub mod session;

pub use authenticator::{
    Authenticator, Clock, ValidateFull, ValidateNothing, ValidateSignature, ValidateTime,
};
pub use certificate::{
    Certificate, CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey,
    NOISE_SIG_MSG_SIZE, NOISE_SIG_SIZE,
};
pub use error::{AuthenticationError, InitializationError, KeyParseError};
pub use handshake::{EstablishedSession, accept, initiate};
pub use keys::{PublicKey, SecretKey, SessionId, Signature};
pub use session::{EncryptedCodec, SessionError};

pub use ii_sv2_crypto::{
    CryptoCtx, Rng, Secp256k1KeyPair, SecretSecp256k1Key, new_keypair, new_private_key,
};

// Re-export secp256k1 itself so downstream callers naming
// `CryptoCtx<secp256k1::AllPreallocated<'_>>` resolve to the same
// version we compiled against, eliminating any version-skew hazard.
pub use secp256k1;

// Convenience: `AlignedType` is the buffer cell type for
// `CryptoCtx::preallocated`. Lives at `secp256k1::ffi::types::AlignedType`
// upstream (the bare `secp256k1::AlignedType` re-export is alloc-gated).
pub use secp256k1::ffi::types::AlignedType;
