#![no_std]

//! User-friendly Stratum V2 security types.
//!
//! Wraps [`ii_sv2_crypto`] with the SV2-spec key encodings (base58check
//! with the `priv` / version-1 prefixes), certificate types, and an
//! async, runtime-agnostic handshake driver. `no_std`, no-alloc.

pub mod authenticator;
pub mod certificate;
pub mod error;
pub mod keys;

pub use authenticator::{
    Authenticator, Clock, ValidateFull, ValidateNothing, ValidateSignature, ValidateTime,
};
pub use certificate::{
    Certificate, CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey,
    NOISE_SIG_MSG_SIZE, NOISE_SIG_SIZE,
};
pub use error::{AuthenticationError, KeyParseError};
pub use keys::{PublicKey, SecretKey, SessionId, Signature};
