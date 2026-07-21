// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Post-handshake noise transport: bidirectional AEAD-encrypted stream.

use crate::{
    MAC_SIZE,
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
    pub(crate) fn new(
        remote_static_pubkey: Option<PublicSecp256k1Key>,
        handshake_hash: [u8; 32],
        sealing_state: SealingCipherState,
        opening_state: OpeningCipherState,
    ) -> Self {
        Self {
            remote_static_pubkey,
            handshake_hash,
            sealing_state,
            opening_state,
        }
    }

    pub fn remote_static_pubkey(&self) -> Option<[u8; 32]> {
        self.remote_static_pubkey.map(|x| x.serialize())
    }

    pub fn handshake_hash(&self) -> [u8; 32] {
        self.handshake_hash
    }

    /// Encrypt `payload[..payload_len]` in place, append the MAC, return total length.
    pub fn write_message_inplace(
        &mut self,
        payload: &mut [u8],
        payload_len: usize,
    ) -> Result<usize, AeadError> {
        self.sealing_state
            .encrypt_with_ad(&[], payload, payload_len)?;
        Ok(payload_len + MAC_SIZE)
    }

    /// Decrypt `payload` in place (last 16 bytes are the MAC), return plaintext length.
    pub fn read_message_inplace(&mut self, payload: &mut [u8]) -> Result<usize, AeadError> {
        if payload.len() < MAC_SIZE {
            return Err(AeadError("not enough space for MAC in buffer"));
        }
        self.opening_state.decrypt_with_ad(&[], payload)?;
        Ok(payload.len() - MAC_SIZE)
    }

    /// Encrypt `payload` into `message` (MAC included), return ciphertext length.
    pub fn write_message(
        &mut self,
        payload: &[u8],
        message: &mut [u8],
    ) -> Result<usize, AeadError> {
        if message.len() < payload.len() + MAC_SIZE {
            return Err(AeadError("not enough space for MAC in buffer"));
        }
        message[..payload.len()].copy_from_slice(payload);
        self.sealing_state
            .encrypt_with_ad(&[], message, payload.len())?;
        Ok(payload.len() + MAC_SIZE)
    }

    /// Decrypt `payload` (MAC at the tail) into `message`, return plaintext length.
    pub fn read_message(&mut self, payload: &[u8], message: &mut [u8]) -> Result<usize, AeadError> {
        if payload.len() < MAC_SIZE {
            return Err(AeadError("not enough space for MAC in buffer"));
        }
        if message.len() < payload.len() {
            return Err(AeadError("not enough space for payload in buffer"));
        }
        message[..payload.len()].copy_from_slice(payload);
        self.opening_state
            .decrypt_with_ad(&[], &mut message[..payload.len()])?;
        Ok(payload.len() - MAC_SIZE)
    }
}
