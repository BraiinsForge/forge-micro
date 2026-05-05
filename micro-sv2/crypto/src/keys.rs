//! secp256k1 key wrappers used in the SV2 noise handshake.

use crate::{
    SECP256_ELLSWIFT_PUBLIC_KEY_LENGTH, SECP256_SECRET_KEY_LENGTH,
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
