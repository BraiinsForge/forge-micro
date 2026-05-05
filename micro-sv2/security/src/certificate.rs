//! Stratum V2 certificate types.
//!
//! Per the SV2 spec, a noise signature message has this layout:
//!
//! | Field             | Type | Bytes |
//! | ----------------- | ---- | ----- |
//! | `version`         | u16  | 0..2  |
//! | `valid_from`      | u32  | 2..6  |
//! | `not_valid_after` | u32  | 6..10 |
//! | `signature`       | u512 | 10..74 |
//!
//! It is bytes-on-the-wire, not an explicit type. Helpers here serialize
//! and parse it; the static public key the signature commits to is
//! transmitted separately as part of the noise handshake.

use ii_sv2_crypto::Secp256k1KeyPair;
use sha2::{Digest, Sha256};

use crate::keys::{PublicKey, SecretKey, Signature};

/// Plaintext bytes of the noise signature message.
pub const NOISE_SIG_SIZE: usize = 74;

/// Wire bytes of the noise signature message (plaintext + AEAD tag).
pub const NOISE_SIG_MSG_SIZE: usize = NOISE_SIG_SIZE + ii_sv2_crypto::MAC_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificateHeader {
    pub version: u16,
    pub valid_from: u32,
    pub not_valid_after: u32,
    pub remote_static_public_key: PublicKey,
}

impl CertificateHeader {
    /// Compute the SHA-256 hash that the authority signs over.
    pub fn hash(&self) -> [u8; 32] {
        let mut buf = [0_u8; 42];
        buf[..2].copy_from_slice(&self.version.to_le_bytes());
        buf[2..6].copy_from_slice(&self.valid_from.to_le_bytes());
        buf[6..10].copy_from_slice(&self.not_valid_after.to_le_bytes());
        buf[10..42].copy_from_slice(self.remote_static_public_key.as_ref());
        Sha256::digest(buf).into()
    }
}

/// A certificate as observed by the client: header + authority pubkey + signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Certificate {
    pub header: CertificateHeader,
    pub authority_public_key: PublicKey,
    pub signature: Signature,
}

impl Certificate {
    /// Serialize the public part of this certificate into its noise
    /// signature message form (74 bytes: version, validity window, and
    /// signature; the static public key is conveyed via the noise
    /// handshake itself).
    ///
    /// Panics if `output.len() < NOISE_SIG_SIZE`.
    pub(crate) fn serialize_noise_signature_msg(&self, output: &mut [u8]) {
        assert!(
            output.len() >= NOISE_SIG_SIZE,
            "BUG: output buffer too small for noise signature message"
        );
        output[..2].copy_from_slice(&self.header.version.to_le_bytes());
        output[2..6].copy_from_slice(&self.header.valid_from.to_le_bytes());
        output[6..10].copy_from_slice(&self.header.not_valid_after.to_le_bytes());
        output[10..NOISE_SIG_SIZE].copy_from_slice(self.signature.as_ref());
    }

    /// Parse the noise signature message bytes the responder sent over the
    /// wire. The remote static public key is supplied separately because it
    /// arrives via the noise handshake itself, not in the signature payload.
    ///
    /// Panics if `data.len() < NOISE_SIG_SIZE` — callers must check.
    pub(crate) fn parse_noise_signature_msg(
        remote_static_public_key: [u8; 32],
        data: &[u8],
    ) -> (CertificateHeader, Signature) {
        assert!(
            data.len() >= NOISE_SIG_SIZE,
            "BUG: data too short for noise signature message"
        );
        let version = u16::from_le_bytes(data[..2].try_into().expect("checked length"));
        let valid_from = u32::from_le_bytes(data[2..6].try_into().expect("checked length"));
        let not_valid_after =
            u32::from_le_bytes(data[6..10].try_into().expect("checked length"));
        let raw_sig: [u8; 64] = data[10..NOISE_SIG_SIZE]
            .try_into()
            .expect("checked length");
        let header = CertificateHeader {
            version,
            valid_from,
            not_valid_after,
            remote_static_public_key: PublicKey::from(remote_static_public_key),
        };
        (header, Signature::from(raw_sig))
    }
}

/// A certificate signing request (CSR) — the header bytes that the
/// authority will sign over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificateSigningRequest {
    pub header: CertificateHeader,
}

impl CertificateSigningRequest {
    /// Sign the CSR with the authority's keypair, producing a Certificate.
    pub fn sign<C: secp256k1::Signing>(
        self,
        authority_private_key: &Secp256k1KeyPair,
        ctx: &secp256k1::Secp256k1<C>,
    ) -> Certificate {
        let authority_public_key = PublicKey::from(authority_private_key);
        let raw_sig = authority_private_key.sign_raw_digest(ctx, self.header.hash());
        Certificate {
            header: self.header,
            authority_public_key,
            signature: Signature::from(raw_sig),
        }
    }
}

/// A certificate plus the responder's static *private* key — what a
/// stratum server needs to accept secure connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificateWithPrivateKey {
    pub header: CertificateHeader,
    pub authority_public_key: PublicKey,
    pub server_static_private_key: SecretKey,
    pub signature: Signature,
}

impl From<CertificateWithPrivateKey> for Certificate {
    fn from(cert: CertificateWithPrivateKey) -> Self {
        Self {
            header: cert.header,
            authority_public_key: cert.authority_public_key,
            signature: cert.signature,
        }
    }
}
