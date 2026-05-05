//! Stratum V2 key types with base58check encoding per the SV2 spec.
//!
//! - `SecretKey` — base58check of `0x109b3700` (big-endian, 4 bytes) + 32
//!   secret bytes. Encoded form always starts with `"priv"`.
//! - `PublicKey` — base58check of `[1, 0]` (little-endian version 1, 2 bytes)
//!   + 32 xonly bytes.
//! - `Signature` — plain base58 (no check), 64 bytes.
//! - `SessionId` — plain base58 (no check), 32 bytes (noise handshake hash).

use core::fmt;
use core::str::FromStr;

use crate::error::KeyParseError;
use ii_sv2_crypto::{PublicSecp256k1Key, Secp256k1KeyPair, SecretSecp256k1Key};

const PRIV_PREFIX: [u8; 4] = 0x109b3700_u32.to_be_bytes();
const PUB_PREFIX: [u8; 2] = [1, 0];

/// Stack buffer big enough to hold any of our base58-encoded keys.
/// Worst case: 36 raw bytes + 4 check ≈ 55 base58 chars. 64 is safe.
const ENCODED_MAX: usize = 64;
/// Stack buffer for a 64-byte signature in plain base58 (~88 chars).
const SIG_ENCODED_MAX: usize = 96;

#[derive(Copy, Clone, PartialEq, Ord, PartialOrd, Eq)]
pub struct SecretKey([u8; 32]);

#[derive(Debug, Copy, Clone, PartialEq, Ord, PartialOrd, Eq)]
pub struct PublicKey([u8; 32]);

#[derive(Debug, Copy, Clone, PartialEq, Ord, PartialOrd, Eq)]
pub struct Signature([u8; 64]);

#[derive(Debug, Copy, Clone, PartialEq, Ord, PartialOrd, Eq, Hash)]
pub struct SessionId(pub [u8; 32]);

macro_rules! basic_impl {
    ($item:ty, $size:literal) => {
        impl AsRef<[u8]> for $item {
            fn as_ref(&self) -> &[u8] {
                self.0.as_slice()
            }
        }
        impl From<[u8; $size]> for $item {
            fn from(value: [u8; $size]) -> Self {
                Self(value)
            }
        }
        impl From<$item> for [u8; $size] {
            fn from(value: $item) -> Self {
                value.0
            }
        }
        impl<'a> TryFrom<&'a [u8]> for $item {
            type Error = <[u8; $size] as TryFrom<&'a [u8]>>::Error;
            fn try_from(value: &'a [u8]) -> Result<Self, Self::Error> {
                Ok(Self(<[u8; $size]>::try_from(value)?))
            }
        }
        impl $item {
            pub const fn new(inner: [u8; $size]) -> Self {
                Self(inner)
            }
        }
    };
}

basic_impl!(SecretKey, 32);
basic_impl!(PublicKey, 32);
basic_impl!(Signature, 64);
basic_impl!(SessionId, 32);

// ---- base58check (Public / Secret) -------------------------------------

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut tmp = [0u8; 34];
        tmp[..2].copy_from_slice(&PUB_PREFIX);
        tmp[2..].copy_from_slice(&self.0);
        encode_with_check(&tmp, f)
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut tmp = [0u8; 36];
        tmp[..4].copy_from_slice(&PRIV_PREFIX);
        tmp[4..].copy_from_slice(&self.0);
        encode_with_check(&tmp, f)
    }
}

impl FromStr for PublicKey {
    type Err = KeyParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // The decoder writes raw_bytes + 4 check bytes into the buffer
        // before stripping; size for the full pre-strip width.
        let mut buf = [0u8; 64];
        let n = bs58::decode(s)
            .with_check(None)
            .onto(&mut buf[..])
            .map_err(|_| KeyParseError::InvalidChecksum)?;
        if n != 34 || buf[..2] != PUB_PREFIX {
            return Err(KeyParseError::NotAPublicKey);
        }
        let key: [u8; 32] = buf[2..34].try_into().expect("checked length");
        Ok(Self(key))
    }
}

impl FromStr for SecretKey {
    type Err = KeyParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !s.starts_with("priv") {
            return Err(KeyParseError::NotAPrivateKey);
        }
        let mut buf = [0u8; 64];
        let n = bs58::decode(s)
            .with_check(None)
            .onto(&mut buf[..])
            .map_err(|_| KeyParseError::InvalidChecksum)?;
        if n != 36 || buf[..4] != PRIV_PREFIX {
            return Err(KeyParseError::NotAPrivateKey);
        }
        let key: [u8; 32] = buf[4..36].try_into().expect("checked length");
        Ok(Self(key))
    }
}

// ---- plain base58 (Signature / SessionId) ------------------------------

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        encode_plain(&self.0, f)
    }
}

impl FromStr for SessionId {
    type Err = KeyParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut buf = [0u8; 32];
        let n = bs58::decode(s)
            .onto(&mut buf[..])
            .map_err(|_| KeyParseError::InvalidChecksum)?;
        if n != 32 {
            return Err(KeyParseError::BadFormat);
        }
        Ok(Self(buf))
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        encode_plain(&self.0, f)
    }
}

impl FromStr for Signature {
    type Err = KeyParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut buf = [0u8; 64];
        let n = bs58::decode(s)
            .onto(&mut buf[..])
            .map_err(|_| KeyParseError::InvalidChecksum)?;
        if n != 64 {
            return Err(KeyParseError::BadFormat);
        }
        Ok(Self(buf))
    }
}

fn encode_with_check(input: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut out = [0u8; ENCODED_MAX];
    let n = bs58::encode(input)
        .with_check()
        .onto(&mut out[..])
        .map_err(|_| fmt::Error)?;
    let s = core::str::from_utf8(&out[..n]).map_err(|_| fmt::Error)?;
    f.write_str(s)
}

fn encode_plain(input: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut out = [0u8; SIG_ENCODED_MAX];
    let n = bs58::encode(input)
        .onto(&mut out[..])
        .map_err(|_| fmt::Error)?;
    let s = core::str::from_utf8(&out[..n]).map_err(|_| fmt::Error)?;
    f.write_str(s)
}

// ---- Debug on SecretKey hides the bytes --------------------------------

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PrivateKey").field(&"<secret>").finish()
    }
}

// ---- Conversions to/from the underlying secp256k1 wrappers -------------

impl From<SecretSecp256k1Key> for SecretKey {
    fn from(value: SecretSecp256k1Key) -> Self {
        Self(value.serialize())
    }
}
impl From<&SecretSecp256k1Key> for SecretKey {
    fn from(value: &SecretSecp256k1Key) -> Self {
        Self(value.serialize())
    }
}
impl From<PublicSecp256k1Key> for PublicKey {
    fn from(value: PublicSecp256k1Key) -> Self {
        Self(value.serialize())
    }
}
impl From<&PublicSecp256k1Key> for PublicKey {
    fn from(value: &PublicSecp256k1Key) -> Self {
        Self(value.serialize())
    }
}
impl From<&Secp256k1KeyPair> for PublicKey {
    fn from(value: &Secp256k1KeyPair) -> Self {
        Self(value.xonly_public_key_bytes())
    }
}
impl From<&Secp256k1KeyPair> for SecretKey {
    fn from(value: &Secp256k1KeyPair) -> Self {
        Self(value.secret_key_bytes())
    }
}
impl From<secp256k1::schnorr::Signature> for Signature {
    fn from(sig: secp256k1::schnorr::Signature) -> Self {
        Self(sig.to_byte_array())
    }
}

impl PublicKey {
    /// Derive the public key from a secret key. Replaces the
    /// allocating `TryFrom<SecretKey>` from the reference library —
    /// callers supply their own secp256k1 context.
    pub fn from_secret_key<C: secp256k1::Signing>(
        secret: &SecretKey,
        ctx: &secp256k1::Secp256k1<C>,
    ) -> Result<Self, KeyParseError> {
        let secp_secret = SecretSecp256k1Key::try_from(secret.0)?;
        let secp_pub = PublicSecp256k1Key::from_secret_key(&secp_secret, ctx);
        Ok(Self(secp_pub.serialize()))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;

    #[test]
    fn encoding_prefixes() {
        // The 4-byte 0x109b3700 prefix was chosen so the bs58check
        // representation always starts with "priv", regardless of payload.
        assert!(SecretKey([0u8; 32]).to_string().starts_with("priv"));
        assert!(SecretKey([0xFFu8; 32]).to_string().starts_with("priv"));
    }

    #[test]
    fn public_key_encoding_roundtrip() {
        let raw = rand::random::<[u8; 32]>();
        let pk = PublicKey(raw);
        let encoded = pk.to_string();
        let parsed: PublicKey = encoded.parse().expect("parse roundtrip");
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn secret_key_encoding_roundtrip() {
        let raw = rand::random::<[u8; 32]>();
        let sk = SecretKey(raw);
        let encoded = sk.to_string();
        assert!(encoded.starts_with("priv"));
        let parsed: SecretKey = encoded.parse().expect("parse roundtrip");
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn signature_encoding_roundtrip() {
        let raw: [u8; 64] = core::array::from_fn(|_| rand::random());
        let sig = Signature(raw);
        let encoded = sig.to_string();
        let parsed: Signature = encoded.parse().expect("parse roundtrip");
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn session_id_encoding_roundtrip() {
        let raw = rand::random::<[u8; 32]>();
        let sid = SessionId(raw);
        let encoded = sid.to_string();
        let parsed: SessionId = encoded.parse().expect("parse roundtrip");
        assert_eq!(parsed.0, raw);
    }

    /// A public key encoding must NOT parse as a private key, and vice
    /// versa. This is the property the prefix bytes are there to enforce.
    #[test]
    fn key_coding_consistency() {
        let raw = rand::random::<[u8; 32]>();
        let pk_str = PublicKey(raw).to_string();
        let sk_str = SecretKey(raw).to_string();
        assert_eq!(
            pk_str.parse::<SecretKey>().unwrap_err(),
            KeyParseError::NotAPrivateKey
        );
        assert_eq!(
            sk_str.parse::<PublicKey>().unwrap_err(),
            KeyParseError::NotAPublicKey
        );
    }

    /// `PublicKey::from_secret_key(sk, ctx)` must agree with deriving
    /// the public via the underlying ii-sv2-crypto wrappers.
    #[test]
    fn conversion_consistency() {
        let ctx = secp256k1::Secp256k1::new();
        let sk = SecretKey(rand::random::<[u8; 32]>());

        // Path 1: through the security-crate convenience method.
        let pk_via_method = PublicKey::from_secret_key(&sk, &ctx).expect("derive pubkey");

        // Path 2: drop down to the secp256k1 wrappers directly.
        let secp_secret = SecretSecp256k1Key::try_from(sk.0).expect("valid secret");
        let secp_pub = PublicSecp256k1Key::from_secret_key(&secp_secret, &ctx);
        let pk_direct = PublicKey::from(secp_pub);

        assert_eq!(pk_via_method, pk_direct);
    }
}

