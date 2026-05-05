//! Post-handshake noise transport: bidirectional AEAD-encrypted stream.

use crate::{
    aead::{OpeningCipherState, SealingCipherState},
    error::AeadError,
    keys::PublicSecp256k1Key,
};

pub struct TransportState {
    pub(crate) remote_static_pubkey: Option<PublicSecp256k1Key>,
    pub(crate) handshake_hash: [u8; 32],
    pub(crate) sealing_state: SealingCipherState,
    pub(crate) opening_state: OpeningCipherState,
}

impl TransportState {
    pub fn remote_static_pubkey(&self) -> Option<[u8; 32]> {
        todo!()
    }

    pub fn handshake_hash(&self) -> [u8; 32] {
        todo!()
    }

    /// Encrypt `payload[..payload_len]` in place, append the MAC, return total length.
    pub fn write_message_inplace(
        &mut self,
        _payload: &mut [u8],
        _payload_len: usize,
    ) -> Result<usize, AeadError> {
        todo!()
    }

    /// Decrypt `payload` in place (last 16 bytes are the MAC), return plaintext length.
    pub fn read_message_inplace(&mut self, _payload: &mut [u8]) -> Result<usize, AeadError> {
        todo!()
    }

    /// Encrypt `payload` into `message` (MAC included), return ciphertext length.
    pub fn write_message(
        &mut self,
        _payload: &[u8],
        _message: &mut [u8],
    ) -> Result<usize, AeadError> {
        todo!()
    }

    /// Decrypt `payload` (MAC at the tail) into `message`, return plaintext length.
    pub fn read_message(
        &mut self,
        _payload: &[u8],
        _message: &mut [u8],
    ) -> Result<usize, AeadError> {
        todo!()
    }
}
