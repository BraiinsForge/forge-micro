//! secp256k1 key wrappers used in the SV2 noise handshake.

use crate::{
    ECDH_LENGTH, SECP256_ELLSWIFT_PUBLIC_KEY_LENGTH, SECP256_SECRET_KEY_LENGTH,
    SECP256_XONLY_PUBLIC_KEY_LENGTH, error::Secp256k1KeyError,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SecretSecp256k1Key(pub(crate) secp256k1::SecretKey);

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicSecp256k1Key(pub(crate) secp256k1::XOnlyPublicKey);

pub type Secp256k1Elligator = secp256k1::ellswift::ElligatorSwift;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Secp256k1KeyPair {
    pub(crate) sk: secp256k1::SecretKey,
    pub(crate) pk: secp256k1::XOnlyPublicKey,
    pub(crate) elligator: Secp256k1Elligator,
}

impl SecretSecp256k1Key {
    pub fn serialize(&self) -> [u8; SECP256_SECRET_KEY_LENGTH] {
        todo!()
    }
}

impl PublicSecp256k1Key {
    pub fn serialize(&self) -> [u8; SECP256_XONLY_PUBLIC_KEY_LENGTH] {
        todo!()
    }

    /// Derive the x-only public key from a secret key.
    pub fn from_secret_key<C: secp256k1::Signing>(
        _secret: &SecretSecp256k1Key,
        _ctx: &secp256k1::Secp256k1<C>,
    ) -> Self {
        todo!()
    }
}

impl From<Secp256k1Elligator> for PublicSecp256k1Key {
    fn from(_value: Secp256k1Elligator) -> Self {
        todo!()
    }
}

impl Secp256k1KeyPair {
    pub fn ellswift_bytes(&self) -> [u8; SECP256_ELLSWIFT_PUBLIC_KEY_LENGTH] {
        todo!()
    }

    pub fn secret_key_bytes(&self) -> [u8; SECP256_SECRET_KEY_LENGTH] {
        todo!()
    }

    pub fn xonly_public_key_bytes(&self) -> [u8; SECP256_XONLY_PUBLIC_KEY_LENGTH] {
        todo!()
    }

    pub fn sign_raw_digest<C: secp256k1::Signing>(
        &self,
        _ctx: &secp256k1::Secp256k1<C>,
        _msg_hash: [u8; 32],
    ) -> secp256k1::schnorr::Signature {
        todo!()
    }

    /// BIP-324 v2 ECDH between this party's elligator and the remote's,
    /// using `self.sk`. The `initiator` flag distinguishes the role; the
    /// operation is non-commutative across roles.
    ///
    /// See https://github.com/bitcoin/bips/blob/master/bip-0324.mediawiki#shared-secret-computation
    pub(crate) fn dh(
        &self,
        _remote_elligator: Secp256k1Elligator,
        _initiator: bool,
    ) -> [u8; ECDH_LENGTH] {
        todo!()
    }
}

impl TryFrom<[u8; SECP256_SECRET_KEY_LENGTH]> for SecretSecp256k1Key {
    type Error = Secp256k1KeyError;
    fn try_from(_value: [u8; SECP256_SECRET_KEY_LENGTH]) -> Result<Self, Self::Error> {
        todo!()
    }
}

impl TryFrom<[u8; SECP256_XONLY_PUBLIC_KEY_LENGTH]> for PublicSecp256k1Key {
    type Error = Secp256k1KeyError;
    fn try_from(_value: [u8; SECP256_XONLY_PUBLIC_KEY_LENGTH]) -> Result<Self, Self::Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keygen::KeyGen;

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
    fn keys_and_conversions() {
        let mut rng = SimpleKeyGen::new();
        for _ in 0..100 {
            let secret1 = rng.new_private_key();
            let public1 = PublicSecp256k1Key::from_secret_key(&secret1, rng.ctx());

            let secret2 = SecretSecp256k1Key::try_from(secret1.serialize())
                .expect("BUG: round-trip failure");
            let public2 = PublicSecp256k1Key::from_secret_key(&secret2, rng.ctx());

            assert_eq!(secret1.0, secret2.0);
            assert_eq!(secret1.serialize(), secret2.serialize());
            assert_eq!(public1.serialize(), public2.serialize());
        }
    }

    #[test]
    fn ecdh_commutativity() {
        let mut rng = SimpleKeyGen::new();
        let alice = rng.new_keypair();
        let bob = rng.new_keypair();

        let alice_ecdh = alice.dh(bob.elligator, true);
        let bob_ecdh = bob.dh(alice.elligator, false);
        let bob_ecdh_wrong_role = bob.dh(alice.elligator, true);

        assert_eq!(alice_ecdh, bob_ecdh);
        assert_ne!(alice_ecdh, bob_ecdh_wrong_role);
    }
}
