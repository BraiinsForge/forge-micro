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
    /// State machine reached an unexpected state.
    InvalidState,
    /// Input buffer is shorter than the expected handshake message.
    NotEnoughBytes,
    /// Output buffer is too small to hold the decrypted payload.
    TooSmallBuffer,
    /// `finish()` was called after a failed handshake.
    FailedHandshake,
    /// `finish()` was called before the handshake was complete.
    UnfinishedHandshake,
    Aead(AeadError),
    Key(Secp256k1KeyError),
    Secp256k1,
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

impl From<secp256k1::Error> for NXError {
    fn from(_: secp256k1::Error) -> Self {
        Self::Secp256k1
    }
}
