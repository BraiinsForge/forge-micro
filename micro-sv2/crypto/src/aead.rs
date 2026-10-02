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

//! ChaCha20-Poly1305 AEAD wrappers used by the noise transport layer.

use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce, Tag,
    aead::{AeadInOut, inout::InOutBuf},
};

use crate::{MAC_SIZE, error::AeadError};

pub(crate) struct SealingCipherState {
    cipher: ChaCha20Poly1305,
    nonce: u64,
}

impl SealingCipherState {
    pub fn initialize(key: [u8; 32]) -> Result<Self, AeadError> {
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| AeadError("broken key"))?;
        Ok(Self { cipher, nonce: 0 })
    }

    /// Encrypts `data[..data_len]` in place and appends the 16-byte tag at
    /// `data[data_len..data_len + MAC_SIZE]`.
    pub fn encrypt_with_ad(
        &mut self,
        ad: &[u8],
        data: &mut [u8],
        data_len: usize,
    ) -> Result<(), AeadError> {
        if data.len() < data_len + MAC_SIZE {
            return Err(AeadError("not enough space for MAC in buffer"));
        }
        let nonce_bytes = expand_nonce(self.nonce);
        self.nonce = self
            .nonce
            .checked_add(1)
            .ok_or(AeadError("nonce overflow"))?;
        let nonce = Nonce::from(nonce_bytes);
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, ad, InOutBuf::from(&mut data[..data_len]))
            .map_err(|_| AeadError("encryption failed"))?;
        data[data_len..data_len + MAC_SIZE].copy_from_slice(tag.as_slice());
        Ok(())
    }
}

pub(crate) struct OpeningCipherState {
    cipher: ChaCha20Poly1305,
    nonce: u64,
}

impl OpeningCipherState {
    pub fn initialize(key: [u8; 32]) -> Result<Self, AeadError> {
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| AeadError("broken key"))?;
        Ok(Self { cipher, nonce: 0 })
    }

    /// Decrypts `data` in place; the trailing 16 bytes are consumed as the tag.
    pub fn decrypt_with_ad(&mut self, ad: &[u8], data: &mut [u8]) -> Result<(), AeadError> {
        if data.len() < MAC_SIZE {
            return Err(AeadError("not enough space for MAC in buffer"));
        }
        let nonce_bytes = expand_nonce(self.nonce);
        self.nonce = self
            .nonce
            .checked_add(1)
            .ok_or(AeadError("nonce overflow"))?;
        let nonce = Nonce::from(nonce_bytes);
        let split = data.len() - MAC_SIZE;
        let (ciphertext, tag_bytes) = data.split_at_mut(split);
        let tag = Tag::try_from(&*tag_bytes).map_err(|_| AeadError("malformed tag"))?;
        self.cipher
            .decrypt_inout_detached(&nonce, ad, InOutBuf::from(ciphertext), &tag)
            .map_err(|_| AeadError("decryption failed"))?;
        Ok(())
    }
}

/// Noise ChaCha20-Poly1305 nonce: 32 zero bits || little-endian `n`.
/// See https://noiseprotocol.org/noise.html#the-chachapoly-cipher-functions
pub(crate) fn expand_nonce(nonce: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[4..].copy_from_slice(&nonce.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAC_SIZE;

    const KEY: [u8; 32] = [
        18, 152, 182, 250, 129, 59, 65, 116, 111, 248, 192, 63, 130, 111, 40, 37, 120, 109, 155,
        65, 66, 241, 231, 128, 39, 87, 58, 121, 34, 203, 110, 221,
    ];
    const NONCE: u64 = 42;
    const DATA: &[u8] = b"FOR START UP AND ATC CLEARANCE CONTACT RUZYNE DELIVERY 120.060";
    const CIPHER: &[u8] = &[
        70, 253, 104, 99, 161, 146, 35, 218, 69, 75, 114, 231, 221, 209, 79, 211, 254, 172, 162,
        83, 15, 166, 138, 162, 220, 194, 57, 232, 158, 214, 107, 100, 74, 77, 228, 249, 73, 151,
        93, 26, 232, 56, 232, 155, 34, 236, 58, 154, 164, 33, 245, 115, 51, 185, 3, 77, 1, 216,
        189, 133, 149, 150, 44, 48, 19, 201, 102, 193, 127, 225, 135, 235, 153, 151, 48, 219, 227,
        79,
    ];

    #[test]
    fn expand_nonce_layout() {
        let n = expand_nonce(0x0102_0304_0506_0708);
        assert_eq!(n[..4], [0, 0, 0, 0]);
        assert_eq!(&n[4..], &0x0102_0304_0506_0708u64.to_le_bytes());
    }

    #[test]
    fn symmetric_encryption() {
        let mut cipher = SealingCipherState::initialize(KEY).unwrap();
        let mut buf = [0_u8; 78];
        // The vector pins the ciphertext at nonce NONCE, so the counter
        // is walked there with empty encryptions first.
        for _ in 0..NONCE {
            cipher.encrypt_with_ad(b"", &mut buf, 0).unwrap();
        }
        buf[..62].copy_from_slice(DATA);
        cipher
            .encrypt_with_ad(b"", &mut buf, DATA.len())
            .expect("BUG: encryption failure");
        assert_eq!(&buf[..], CIPHER);
    }

    #[test]
    fn symmetric_decryption() {
        let mut sealing = SealingCipherState::initialize(KEY).unwrap();
        let mut opening = OpeningCipherState::initialize(KEY).unwrap();
        // The vector pins the ciphertext at nonce 42.
        let mut tag = [0_u8; MAC_SIZE];
        for _ in 0..NONCE {
            sealing.encrypt_with_ad(b"", &mut tag, 0).unwrap();
            opening.decrypt_with_ad(b"", &mut tag).unwrap();
        }
        let mut full = [0_u8; 78];
        full.copy_from_slice(CIPHER);
        opening
            .decrypt_with_ad(b"", &mut full)
            .expect("BUG: decryption failure");
        assert_eq!(&full[..DATA.len()], DATA);
    }
}
