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
    error::NXError,
    keygen::KeyGen,
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

pub struct HandShake<R: KeyGen> {
    pub(crate) state: NXState,
    pub(crate) rng: R,
}

impl<R: KeyGen> HandShake<R> {
    pub fn new_initiator(rng: R) -> Self {
        Self {
            state: NXState::InitiatorStep0(InitiatorState0 {
                ck: PROTOCOL_NAME_HASH,
                h: PROTOCOL_NAME_HASH_HASH,
            }),
            rng,
        }
    }

    pub fn new_responder(rng: R, static_keypair: Secp256k1KeyPair) -> Self {
        Self {
            state: NXState::ResponderStep0(ResponderState0 {
                ck: PROTOCOL_NAME_HASH,
                h: PROTOCOL_NAME_HASH_HASH,
                s: static_keypair,
            }),
            rng,
        }
    }

    /// Walk one step of the state machine, writing a handshake message.
    pub fn write_message(
        &mut self,
        _message: &[u8],
        _payload: &mut [u8],
    ) -> Result<usize, NXError> {
        todo!()
    }

    /// Walk one step of the state machine, reading a handshake message.
    pub fn read_message(
        &mut self,
        _payload: &[u8],
        _message: &mut [u8],
    ) -> Result<usize, NXError> {
        todo!()
    }

    /// Consume `self`, returning the post-handshake transport state.
    pub fn finish(self) -> Result<TransportState, NXError> {
        match self.state {
            NXState::Finished(state) => Ok(state),
            NXState::Failed => Err(NXError::FailedHandshake),
            _ => Err(NXError::UnfinishedHandshake),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::PublicSecp256k1Key;
    use rand::{Rng, RngCore};
    use sha2::{Digest, Sha256};

    struct SimpleKeyGen {
        ctx: secp256k1::Secp256k1<secp256k1::All>,
    }

    impl SimpleKeyGen {
        fn new() -> Self {
            Self {
                ctx: secp256k1::Secp256k1::new(),
            }
        }
    }

    impl KeyGen for SimpleKeyGen {
        type Ctx = secp256k1::All;

        fn new_256_bit_entropy(&mut self) -> [u8; 32] {
            rand::random::<[u8; 32]>()
        }

        fn ctx(&self) -> &secp256k1::Secp256k1<Self::Ctx> {
            &self.ctx
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
        let initiator: HandShake<SimpleKeyGen> = HandShake::new_initiator(SimpleKeyGen::new());
        let NXState::InitiatorStep0(s) = &initiator.state else {
            panic!("BUG: initiator must start in InitiatorStep0");
        };
        assert_eq!(s.h, PROTOCOL_NAME_HASH_HASH);
        assert_eq!(s.ck, PROTOCOL_NAME_HASH);

        let mut bootstrap = SimpleKeyGen::new();
        let static_kp = bootstrap.new_keypair();
        let responder: HandShake<SimpleKeyGen> =
            HandShake::new_responder(SimpleKeyGen::new(), static_kp);
        let NXState::ResponderStep0(s) = &responder.state else {
            panic!("BUG: responder must start in ResponderStep0");
        };
        assert_eq!(s.h, PROTOCOL_NAME_HASH_HASH);
        assert_eq!(s.ck, PROTOCOL_NAME_HASH);
    }

    #[test]
    fn handshake_roundtrip() {
        const DUMMY_CERT_LEN: usize = 77;

        let mut bootstrap = SimpleKeyGen::new();
        let static_private = bootstrap.new_keypair();
        let mut initiator = HandShake::new_initiator(SimpleKeyGen::new());
        let mut responder = HandShake::new_responder(SimpleKeyGen::new(), static_private);

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
