//! Static-dispatch authenticator selector.
//!
//! `Sv2Config::pool_pubkey` decides which built-in `Authenticator`
//! the adapter uses. We need a single concrete type to pass to
//! `ii_sv2::security::initiate`, so the two cases are wrapped in
//! one enum that itself implements `Authenticator`.

use ii_sv2::security::{
    AuthenticationError, Authenticator, CertificateHeader, Signature, ValidateNothing,
    ValidateSignature, secp256k1,
};

/// Either no validation (test pools) or signature validation against
/// the configured pool authority public key.
pub enum AuthChoice<'a, C: secp256k1::Verification> {
    Nothing(ValidateNothing),
    Signature(ValidateSignature<'a, C>),
}

impl<C: secp256k1::Verification> Authenticator for AuthChoice<'_, C> {
    fn check(&self, hdr: CertificateHeader, sig: Signature) -> Result<(), AuthenticationError> {
        match self {
            Self::Nothing(v) => v.check(hdr, sig),
            Self::Signature(v) => v.check(hdr, sig),
        }
    }
}
