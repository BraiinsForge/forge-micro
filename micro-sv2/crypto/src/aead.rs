//! ChaCha20-Poly1305 AEAD wrappers used by the noise transport layer.

use crate::error::AeadError;

pub(crate) struct SealingCipherState {
    // TODO: chacha20poly1305::ChaCha20Poly1305 cipher + u64 nonce counter.
}

impl SealingCipherState {
    pub fn initialize(_key: [u8; 32]) -> Result<Self, AeadError> {
        todo!()
    }

    /// Encrypts `data[..data_len]` in place and appends the 16-byte tag at
    /// `data[data_len..data_len + MAC_SIZE]`.
    pub fn encrypt_with_ad(
        &mut self,
        _ad: &[u8],
        _data: &mut [u8],
        _data_len: usize,
    ) -> Result<(), AeadError> {
        todo!()
    }
}

pub(crate) struct OpeningCipherState {
    // TODO: chacha20poly1305::ChaCha20Poly1305 cipher + u64 nonce counter.
}

impl OpeningCipherState {
    pub fn initialize(_key: [u8; 32]) -> Result<Self, AeadError> {
        todo!()
    }

    /// Decrypts `data` in place; the trailing 16 bytes are consumed as the tag.
    pub fn decrypt_with_ad(&mut self, _ad: &[u8], _data: &mut [u8]) -> Result<(), AeadError> {
        todo!()
    }
}

/// Noise ChaCha20-Poly1305 nonce: 32 zero bits || little-endian `n`.
/// See https://noiseprotocol.org/noise.html#the-chachapoly-cipher-functions
pub(crate) fn expand_nonce(nonce: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[4..].copy_from_slice(&nonce.to_le_bytes());
    out
}
