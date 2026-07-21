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
        Self(
            secp256k1::PublicKey::from_ellswift(value)
                .x_only_public_key()
                .0,
        )
    }
}

impl Secp256k1KeyPair {
    /// Reconstruct a keypair from an existing secret key. Mixes in fresh
    /// entropy from `rng` for the elligator encoding.
    pub fn from_secret_key<R: crate::Rng, C: secp256k1::Signing + secp256k1::Verification>(
        secret: &SecretSecp256k1Key,
        rng: &mut R,
        ctx: &crate::CryptoCtx<C>,
    ) -> Self {
        let entropy = rng.next_256_bits();
        let secp = ctx.as_secp256k1();
        let (pk, _) = secret.0.x_only_public_key(secp);
        let elligator =
            secp256k1::ellswift::ElligatorSwift::from_seckey(secp, secret.0, Some(entropy));
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
        let secret =
            ellswift::ElligatorSwift::shared_secret(init_ell, resp_ell, self.sk, party, None);
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

/// Sample a fresh secp256k1 private key. Tries up to 10 times; panics
/// on persistent failure (cryptographically negligible — would mean the
/// `Rng` is broken).
pub fn new_private_key<R: crate::Rng>(rng: &mut R) -> SecretSecp256k1Key {
    for _ in 0..10 {
        let bytes = rng.next_256_bits();
        if let Ok(sk) = secp256k1::SecretKey::from_byte_array(bytes) {
            return SecretSecp256k1Key(sk);
        }
    }
    panic!("BUG: failed to generate secp256k1 private key after 10 tries");
}

/// Sample a fresh keypair (secret + x-only public + ellswift encoding).
pub fn new_keypair<R: crate::Rng, C: secp256k1::Signing + secp256k1::Verification>(
    rng: &mut R,
    ctx: &crate::CryptoCtx<C>,
) -> Secp256k1KeyPair {
    let sk = new_private_key(rng);
    let entropy = rng.next_256_bits();
    let secp = ctx.as_secp256k1();
    let (pk, _) = sk.0.x_only_public_key(secp);
    let elligator = secp256k1::ellswift::ElligatorSwift::from_seckey(secp, sk.0, Some(entropy));
    Secp256k1KeyPair {
        sk: sk.0,
        pk,
        elligator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CryptoCtx, Rng};

    struct MyRng;
    impl Rng for MyRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            use rand::RngCore;
            rand::rng().fill_bytes(dest);
        }
    }

    #[test]
    fn keys_and_conversions() {
        let mut rng = MyRng;
        let ctx = CryptoCtx::new();
        for _ in 0..100 {
            let secret1 = new_private_key(&mut rng);
            let public1 = PublicSecp256k1Key::from_secret_key(&secret1, ctx.as_secp256k1());

            let secret2 =
                SecretSecp256k1Key::try_from(secret1.serialize()).expect("BUG: round-trip failure");
            let public2 = PublicSecp256k1Key::from_secret_key(&secret2, ctx.as_secp256k1());

            assert_eq!(secret1.0, secret2.0);
            assert_eq!(secret1.serialize(), secret2.serialize());
            assert_eq!(public1.serialize(), public2.serialize());
        }
    }

    #[test]
    fn ecdh_commutativity() {
        let mut rng = MyRng;
        let ctx = CryptoCtx::new();
        let alice = new_keypair(&mut rng, &ctx);
        let bob = new_keypair(&mut rng, &ctx);

        let alice_ecdh = alice.dh(bob.elligator, true);
        let bob_ecdh = bob.dh(alice.elligator, false);
        let bob_ecdh_wrong_role = bob.dh(alice.elligator, true);

        assert_eq!(alice_ecdh, bob_ecdh);
        assert_ne!(alice_ecdh, bob_ecdh_wrong_role);
    }
}
