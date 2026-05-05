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
        self.0.secret_bytes()
    }
}

impl PublicSecp256k1Key {
    pub fn serialize(&self) -> [u8; SECP256_XONLY_PUBLIC_KEY_LENGTH] {
        self.0.serialize()
    }

    /// Derive the x-only public key from a secret key.
    pub fn from_secret_key<C: secp256k1::Signing>(
        secret: &SecretSecp256k1Key,
        ctx: &secp256k1::Secp256k1<C>,
    ) -> Self {
        let (pk, _) = secret.0.x_only_public_key(ctx);
        Self(pk)
    }
}

impl From<Secp256k1Elligator> for PublicSecp256k1Key {
    fn from(value: Secp256k1Elligator) -> Self {
        Self(secp256k1::PublicKey::from_ellswift(value).x_only_public_key().0)
    }
}

impl Secp256k1KeyPair {
    /// Reconstruct a keypair from an existing secret key. Mixes in fresh
    /// entropy from the supplied [`KeyGen`] for the elligator encoding.
    pub fn from_secret_key<K: crate::keygen::KeyGen>(
        secret: &SecretSecp256k1Key,
        rng: &mut K,
    ) -> Self {
        let entropy = rng.new_256_bit_entropy();
        let ctx = rng.ctx();
        let (pk, _) = secret.0.x_only_public_key(ctx);
        let elligator =
            secp256k1::ellswift::ElligatorSwift::from_seckey(ctx, secret.0, Some(entropy));
        Self {
            sk: secret.0,
            pk,
            elligator,
        }
    }

    pub fn ellswift_bytes(&self) -> [u8; SECP256_ELLSWIFT_PUBLIC_KEY_LENGTH] {
        self.elligator.to_array()
    }

    pub fn secret_key_bytes(&self) -> [u8; SECP256_SECRET_KEY_LENGTH] {
        self.sk.secret_bytes()
    }

    pub fn xonly_public_key_bytes(&self) -> [u8; SECP256_XONLY_PUBLIC_KEY_LENGTH] {
        self.pk.serialize()
    }

    pub fn sign_raw_digest<C: secp256k1::Signing>(
        &self,
        ctx: &secp256k1::Secp256k1<C>,
        msg_hash: [u8; 32],
    ) -> secp256k1::schnorr::Signature {
        let keypair = secp256k1::Keypair::from_secret_key(ctx, &self.sk);
        let msg = secp256k1::Message::from_digest(msg_hash);
        ctx.sign_schnorr_no_aux_rand(msg.as_ref(), &keypair)
    }

    /// BIP-324 v2 ECDH between this party's elligator and the remote's,
    /// using `self.sk`. The `initiator` flag distinguishes the role; the
    /// operation is non-commutative across roles.
    ///
    /// See https://github.com/bitcoin/bips/blob/master/bip-0324.mediawiki#shared-secret-computation
    pub(crate) fn dh(
        &self,
        remote_elligator: Secp256k1Elligator,
        initiator: bool,
    ) -> [u8; ECDH_LENGTH] {
        use secp256k1::ellswift;
        let (init_ell, resp_ell, party) = if initiator {
            (self.elligator, remote_elligator, ellswift::Party::Initiator)
        } else {
            (remote_elligator, self.elligator, ellswift::Party::Responder)
        };
        let secret = ellswift::ElligatorSwift::shared_secret(init_ell, resp_ell, self.sk, party, None);
        secret.to_secret_bytes()
    }
}

impl TryFrom<[u8; SECP256_SECRET_KEY_LENGTH]> for SecretSecp256k1Key {
    type Error = Secp256k1KeyError;
    fn try_from(value: [u8; SECP256_SECRET_KEY_LENGTH]) -> Result<Self, Self::Error> {
        secp256k1::SecretKey::from_byte_array(value)
            .map(Self)
            .map_err(|_| Secp256k1KeyError::InvalidSecretKey)
    }
}

impl TryFrom<[u8; SECP256_XONLY_PUBLIC_KEY_LENGTH]> for PublicSecp256k1Key {
    type Error = Secp256k1KeyError;
    fn try_from(value: [u8; SECP256_XONLY_PUBLIC_KEY_LENGTH]) -> Result<Self, Self::Error> {
        secp256k1::XOnlyPublicKey::from_byte_array(value)
            .map(Self)
            .map_err(|_| Secp256k1KeyError::InvalidPublicKey)
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
