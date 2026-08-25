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

//! Noise NX handshake state machine.
//!
//! Pattern:
//! ```text
//! Noise_NX(rs):
//!   -> e
//!   <- e, ee, s, es
//! ```

use crate::{
    MAC_SIZE,
    aead::{OpeningCipherState, SealingCipherState},
    error::NXError,
    hash::{hkdf, mix_hash},
    keys::{Secp256k1Elligator, Secp256k1KeyPair},
    transport::TransportState,
};

/// Noise protocol identifier.
pub const PROTOCOL_NAME: &str = "Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256";

/// Pre-computed `SHA-256(PROTOCOL_NAME)`.
pub const PROTOCOL_NAME_HASH: [u8; 32] = [
    46, 180, 120, 129, 32, 142, 158, 238, 31, 102, 159, 103, 198, 110, 231, 14, 169, 234, 136, 9,
    13, 80, 63, 232, 48, 220, 75, 200, 62, 41, 191, 16,
];

/// Pre-computed `SHA-256(SHA-256(PROTOCOL_NAME))`.
pub const PROTOCOL_NAME_HASH_HASH: [u8; 32] = [
    146, 47, 163, 46, 79, 72, 124, 13, 89, 202, 163, 190, 215, 137, 156, 227, 217, 141, 183, 225,
    61, 189, 59, 124, 242, 210, 61, 212, 51, 220, 97, 4,
];

/// Ellswift-encoded ephemeral key, on the wire.
pub const EPHEMERAL_KEY_SIZE: usize = 64;

/// Ellswift-encoded static key, plaintext length (encrypted on the wire adds
/// `MAC_SIZE`).
pub const STATIC_KEY_SIZE: usize = 64;

/// Wire size of the ephemeral key field.
pub const EPHEMERAL_KEY_MSG_SIZE: usize = EPHEMERAL_KEY_SIZE;

/// Wire size of the encrypted static key field.
pub const STATIC_KEY_MSG_SIZE: usize = STATIC_KEY_SIZE + MAC_SIZE;

pub(crate) struct InitiatorState0 {
    pub(crate) ck: [u8; 32],
    pub(crate) h: [u8; 32],
}

pub(crate) struct InitiatorState1 {
    pub(crate) ck: [u8; 32],
    pub(crate) h: [u8; 32],
    pub(crate) e: Secp256k1KeyPair,
}

pub(crate) struct ResponderState0 {
    pub(crate) ck: [u8; 32],
    pub(crate) h: [u8; 32],
    pub(crate) s: Secp256k1KeyPair,
}

pub(crate) struct ResponderState1 {
    pub(crate) ck: [u8; 32],
    pub(crate) h: [u8; 32],
    pub(crate) s: Secp256k1KeyPair,
    pub(crate) re_pk: Secp256k1Elligator,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum NXState {
    InitiatorStep0(InitiatorState0),
    ResponderStep0(ResponderState0),
    InitiatorStep1(InitiatorState1),
    ResponderStep1(ResponderState1),
    Finished(TransportState),
    Failed,
}

pub struct HandShake<'ctx, R: crate::Rng, C: secp256k1::Signing + secp256k1::Verification> {
    pub(crate) state: NXState,
    pub(crate) rng: R,
    pub(crate) ctx: &'ctx crate::CryptoCtx<C>,
}

impl<'ctx, R: crate::Rng, C: secp256k1::Signing + secp256k1::Verification> HandShake<'ctx, R, C> {
    pub fn new_initiator(rng: R, ctx: &'ctx crate::CryptoCtx<C>) -> Self {
        Self {
            state: NXState::InitiatorStep0(InitiatorState0 {
                ck: PROTOCOL_NAME_HASH,
                h: PROTOCOL_NAME_HASH_HASH,
            }),
            rng,
            ctx,
        }
    }

    pub fn new_responder(
        rng: R,
        ctx: &'ctx crate::CryptoCtx<C>,
        static_keypair: Secp256k1KeyPair,
    ) -> Self {
        Self {
            state: NXState::ResponderStep0(ResponderState0 {
                ck: PROTOCOL_NAME_HASH,
                h: PROTOCOL_NAME_HASH_HASH,
                s: static_keypair,
            }),
            rng,
            ctx,
        }
    }

    /// Walk one step of the state machine, writing a handshake message.
    pub fn write_message(&mut self, message: &[u8], payload: &mut [u8]) -> Result<usize, NXError> {
        let current = core::mem::replace(&mut self.state, NXState::Failed);
        let mut bytes_written = 0_usize;
        let new_state = match current {
            NXState::InitiatorStep0(s) => {
                let s = self.initiator_step0(s, message, payload, &mut bytes_written)?;
                NXState::InitiatorStep1(s)
            }
            NXState::ResponderStep1(s) => {
                let t = self.responder_step1(s, message, payload, &mut bytes_written)?;
                NXState::Finished(t)
            }
            _ => return Err(NXError::InvalidState),
        };
        self.state = new_state;
        Ok(bytes_written)
    }

    /// Walk one step of the state machine, reading a handshake message.
    pub fn read_message(&mut self, payload: &[u8], message: &mut [u8]) -> Result<usize, NXError> {
        let current = core::mem::replace(&mut self.state, NXState::Failed);
        let mut bytes_written = 0_usize;
        let new_state = match current {
            NXState::ResponderStep0(s) => {
                let s = self.responder_step0(s, payload, message, &mut bytes_written)?;
                NXState::ResponderStep1(s)
            }
            NXState::InitiatorStep1(s) => {
                let t = self.initiator_step1(s, payload, message, &mut bytes_written)?;
                NXState::Finished(t)
            }
            _ => return Err(NXError::InvalidState),
        };
        self.state = new_state;
        Ok(bytes_written)
    }

    /// Consume `self`, returning the post-handshake transport state.
    pub fn finish(self) -> Result<TransportState, NXError> {
        match self.state {
            NXState::Finished(state) => Ok(state),
            NXState::Failed => Err(NXError::FailedHandshake),
            _ => Err(NXError::UnfinishedHandshake),
        }
    }

    fn initiator_step0(
        &mut self,
        state: InitiatorState0,
        message: &[u8],
        payload: &mut [u8],
        bytes_written: &mut usize,
    ) -> Result<InitiatorState1, NXError> {
        let InitiatorState0 { h, ck } = state;
        if payload.len() < EPHEMERAL_KEY_MSG_SIZE + message.len() {
            return Err(NXError::TooSmallBuffer);
        }
        let e = crate::keys::Secp256k1KeyPair::generate(&mut self.rng, self.ctx);
        let raw_e = e.ellswift_bytes();
        let h = mix_hash(&h, &raw_e);
        let h = mix_hash(&h, message);
        payload[..EPHEMERAL_KEY_MSG_SIZE].copy_from_slice(&raw_e);
        payload[EPHEMERAL_KEY_MSG_SIZE..EPHEMERAL_KEY_MSG_SIZE + message.len()]
            .copy_from_slice(message);
        *bytes_written += EPHEMERAL_KEY_MSG_SIZE + message.len();
        Ok(InitiatorState1 { h, ck, e })
    }

    fn initiator_step1(
        &mut self,
        state: InitiatorState1,
        payload: &[u8],
        message: &mut [u8],
        bytes_written: &mut usize,
    ) -> Result<TransportState, NXError> {
        if payload.len() < EPHEMERAL_KEY_MSG_SIZE + STATIC_KEY_MSG_SIZE {
            return Err(NXError::NotEnoughBytes);
        }
        let encrypted_data_len = payload.len() - EPHEMERAL_KEY_MSG_SIZE - STATIC_KEY_MSG_SIZE;
        if encrypted_data_len < MAC_SIZE {
            return Err(NXError::NotEnoughBytes);
        }
        if message.len() < encrypted_data_len {
            return Err(NXError::TooSmallBuffer);
        }
        let InitiatorState1 { h, ck, e } = state;
        let (raw_ek, rest) = payload.split_at(EPHEMERAL_KEY_MSG_SIZE);
        let (raw_sk, raw_sig_noise_msg) = rest.split_at(STATIC_KEY_MSG_SIZE);

        let re_pk = expect_elligator(raw_ek);

        // ee
        let h = mix_hash(&h, raw_ek);
        let ee = e.dh(re_pk, true);
        let (ck, k) = hkdf(&ck, &ee);
        let mut cipher = OpeningCipherState::initialize(k)?;

        // es
        let mut static_pk = [0_u8; STATIC_KEY_MSG_SIZE];
        static_pk.copy_from_slice(raw_sk);
        cipher.decrypt_with_ad(&h, &mut static_pk)?;
        let h = mix_hash(&h, raw_sk);
        let rs_pk = expect_elligator(&static_pk[..STATIC_KEY_SIZE]);
        let es = e.dh(rs_pk, true);
        let (ck, k) = hkdf(&ck, &es);
        let mut cipher = OpeningCipherState::initialize(k)?;

        // authenticated payload (cert)
        let buf = &mut message[..encrypted_data_len];
        buf.copy_from_slice(raw_sig_noise_msg);
        let handshake_hash = mix_hash(&h, buf);
        cipher.decrypt_with_ad(&h, buf)?;
        *bytes_written += encrypted_data_len - MAC_SIZE;

        // Split: initiator's sealing direction is initiator -> responder.
        let (k_i_to_r, k_r_to_i) = hkdf(&ck, &[]);
        let sealing_state = SealingCipherState::initialize(k_i_to_r)?;
        let opening_state = OpeningCipherState::initialize(k_r_to_i)?;

        Ok(TransportState::new(
            Some(rs_pk.into()),
            handshake_hash,
            sealing_state,
            opening_state,
        ))
    }

    fn responder_step0(
        &mut self,
        state: ResponderState0,
        payload: &[u8],
        message: &mut [u8],
        bytes_written: &mut usize,
    ) -> Result<ResponderState1, NXError> {
        if payload.len() < EPHEMERAL_KEY_MSG_SIZE {
            return Err(NXError::NotEnoughBytes);
        }
        let ResponderState0 { h, ck, s } = state;
        let raw_e = &payload[..EPHEMERAL_KEY_MSG_SIZE];
        let re_pk = expect_elligator(raw_e);
        let h = mix_hash(&h, raw_e);
        let plaintext = &payload[EPHEMERAL_KEY_MSG_SIZE..];
        if message.len() < plaintext.len() {
            return Err(NXError::TooSmallBuffer);
        }
        let h = mix_hash(&h, plaintext);
        message[..plaintext.len()].copy_from_slice(plaintext);
        *bytes_written += plaintext.len();
        Ok(ResponderState1 { h, ck, s, re_pk })
    }

    fn responder_step1(
        &mut self,
        state: ResponderState1,
        message: &[u8],
        payload: &mut [u8],
        bytes_written: &mut usize,
    ) -> Result<TransportState, NXError> {
        let ResponderState1 { h, ck, s, re_pk } = state;
        let total = EPHEMERAL_KEY_MSG_SIZE + STATIC_KEY_MSG_SIZE + message.len() + MAC_SIZE;
        if payload.len() < total {
            return Err(NXError::TooSmallBuffer);
        }
        let mut bytes_processed = 0_usize;

        let e = crate::keys::Secp256k1KeyPair::generate(&mut self.rng, self.ctx);
        let raw_e = e.ellswift_bytes();
        payload[..EPHEMERAL_KEY_MSG_SIZE].copy_from_slice(&raw_e);
        bytes_processed += EPHEMERAL_KEY_MSG_SIZE;

        // ee
        let h = mix_hash(&h, &raw_e);
        let ee = e.dh(re_pk, false);
        let (ck, k) = hkdf(&ck, &ee);
        let mut cipher = SealingCipherState::initialize(k)?;

        // encrypt static pubkey under nonce 0
        let mut static_pk = [0_u8; STATIC_KEY_MSG_SIZE];
        static_pk[..STATIC_KEY_SIZE].copy_from_slice(&s.ellswift_bytes());
        cipher.encrypt_with_ad(&h, &mut static_pk, STATIC_KEY_SIZE)?;
        let h = mix_hash(&h, &static_pk);
        payload[bytes_processed..bytes_processed + STATIC_KEY_MSG_SIZE].copy_from_slice(&static_pk);
        bytes_processed += STATIC_KEY_MSG_SIZE;

        // es
        let es = s.dh(re_pk, false);
        let (ck, k) = hkdf(&ck, &es);
        let mut cipher = SealingCipherState::initialize(k)?;
        let msg_size = message.len() + MAC_SIZE;
        let outbuf = &mut payload[bytes_processed..bytes_processed + msg_size];
        outbuf[..message.len()].copy_from_slice(message);
        cipher.encrypt_with_ad(&h, outbuf, message.len())?;
        let h = mix_hash(&h, outbuf);
        bytes_processed += msg_size;
        *bytes_written += bytes_processed;

        // Split: responder's sealing direction is responder -> initiator.
        let (k_i_to_r, k_r_to_i) = hkdf(&ck, &[]);
        let sealing_state = SealingCipherState::initialize(k_r_to_i)?;
        let opening_state = OpeningCipherState::initialize(k_i_to_r)?;

        Ok(TransportState::new(None, h, sealing_state, opening_state))
    }
}

/// Panics if `raw` is not exactly `EPHEMERAL_KEY_SIZE` bytes — callers
/// must validate length before invoking.
fn expect_elligator(raw: &[u8]) -> Secp256k1Elligator {
    Secp256k1Elligator::from_array(
        raw.try_into()
            .expect("BUG: caller should have checked length"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CryptoCtx, Rng,
        keys::{PublicSecp256k1Key, Secp256k1KeyPair},
    };
    use rand::{Rng as _, RngCore};
    use sha2::{Digest, Sha256};

    struct MyRng;
    impl Rng for MyRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            rand::rng().fill_bytes(dest);
        }
    }

    #[test]
    fn protocol_name_hash_matches_constants() {
        let h = Sha256::digest(PROTOCOL_NAME.as_bytes());
        assert_eq!(h.as_slice(), &PROTOCOL_NAME_HASH);
        let h = Sha256::digest(h);
        assert_eq!(h.as_slice(), &PROTOCOL_NAME_HASH_HASH);
    }

    #[test]
    fn handshake_state_init() {
        let ctx = CryptoCtx::new();
        let initiator = HandShake::new_initiator(MyRng, &ctx);
        let NXState::InitiatorStep0(s) = &initiator.state else {
            panic!("BUG: initiator must start in InitiatorStep0");
        };
        assert_eq!(s.h, PROTOCOL_NAME_HASH_HASH);
        assert_eq!(s.ck, PROTOCOL_NAME_HASH);

        let static_kp = Secp256k1KeyPair::generate(&mut MyRng, &ctx);
        let responder = HandShake::new_responder(MyRng, &ctx, static_kp);
        let NXState::ResponderStep0(s) = &responder.state else {
            panic!("BUG: responder must start in ResponderStep0");
        };
        assert_eq!(s.h, PROTOCOL_NAME_HASH_HASH);
        assert_eq!(s.ck, PROTOCOL_NAME_HASH);
    }

    #[test]
    fn handshake_roundtrip() {
        const DUMMY_CERT_LEN: usize = 77;
        let ctx = CryptoCtx::new();
        let static_private = Secp256k1KeyPair::generate(&mut MyRng, &ctx);
        let mut initiator = HandShake::new_initiator(MyRng, &ctx);
        let mut responder = HandShake::new_responder(MyRng, &ctx, static_private);

        let mut buf = [0_u8; 256];
        let mut buf2 = [0_u8; 256];

        let m1_len = initiator.write_message(b"", &mut buf).expect("BUG");
        assert_eq!(m1_len, EPHEMERAL_KEY_MSG_SIZE);

        let m2_len = responder
            .read_message(&buf[..EPHEMERAL_KEY_MSG_SIZE], &mut [0_u8; MAC_SIZE])
            .expect("BUG");
        assert_eq!(m2_len, 0);

        let dummy_cert = [64_u8; DUMMY_CERT_LEN];
        let expected_m3_len =
            EPHEMERAL_KEY_MSG_SIZE + STATIC_KEY_MSG_SIZE + DUMMY_CERT_LEN + MAC_SIZE;
        let m3_len = responder.write_message(&dummy_cert, &mut buf).expect("BUG");
        assert_eq!(m3_len, expected_m3_len);

        let m4_len = initiator
            .read_message(&buf[..m3_len], &mut buf2)
            .expect("BUG");
        assert_eq!(m4_len, DUMMY_CERT_LEN);
        assert_eq!(&buf2[..DUMMY_CERT_LEN], &dummy_cert[..]);

        let mut r_transport = responder.finish().expect("BUG");
        let mut i_transport = initiator.finish().expect("BUG");
        assert_eq!(r_transport.handshake_hash(), i_transport.handshake_hash());

        let remote_static_pk = i_transport
            .remote_static_pubkey()
            .expect("BUG: initiator should know responder's static key");
        assert_eq!(
            PublicSecp256k1Key::try_from(remote_static_pk).unwrap(),
            PublicSecp256k1Key(static_private.pk),
        );

        let max_pt_len = 256 - MAC_SIZE;
        let mut rng = rand::rng();
        for _ in 0..10 {
            // i -> r
            rng.fill_bytes(&mut buf);
            let msg_len = rng.random_range(0..max_pt_len);
            buf2[..msg_len].copy_from_slice(&buf[..msg_len]);
            i_transport
                .write_message_inplace(&mut buf, msg_len)
                .expect("BUG");
            r_transport
                .read_message_inplace(&mut buf[..msg_len + MAC_SIZE])
                .expect("BUG");
            assert_eq!(&buf[..msg_len], &buf2[..msg_len]);

            // r -> i
            rng.fill_bytes(&mut buf);
            let msg_len = rng.random_range(0..max_pt_len);
            buf2[..msg_len].copy_from_slice(&buf[..msg_len]);
            r_transport
                .write_message_inplace(&mut buf, msg_len)
                .expect("BUG");
            i_transport
                .read_message_inplace(&mut buf[..msg_len + MAC_SIZE])
                .expect("BUG");
            assert_eq!(&buf[..msg_len], &buf2[..msg_len]);
        }
    }
}
