//! Alloc-free error types for the SV2 noise crypto core.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secp256k1KeyError {
    InvalidSecretKey,
    InvalidPublicKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeadError(pub &'static str);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NXError {
    InvalidState,
    InvalidMessageLength,
    Aead(AeadError),
    Key(Secp256k1KeyError),
}

impl From<AeadError> for NXError {
    fn from(e: AeadError) -> Self {
        Self::Aead(e)
    }
}

impl From<Secp256k1KeyError> for NXError {
    fn from(e: Secp256k1KeyError) -> Self {
        Self::Key(e)
    }
}
