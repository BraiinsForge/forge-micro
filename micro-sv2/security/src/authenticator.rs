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

//! Authenticators for SV2 noise certificates.
//!
//! An [`Authenticator`] decides whether a certificate observed during
//! the noise handshake is acceptable. Several stock implementations are
//! provided; users can also implement the trait directly.
//!
//! Time-based validators take a user-supplied [`Clock`] since `no_std`
//! has no system clock.

use crate::certificate::CertificateHeader;
use crate::error::AuthenticationError;
use crate::keys::{PublicKey, Signature};

/// Source of unix-epoch time (seconds), supplied by the embedded host.
pub trait Clock {
    fn now_unix_secs(&self) -> Result<u32, AuthenticationError>;
}

pub trait Authenticator {
    fn check(
        &self,
        certificate_header: CertificateHeader,
        signature: Signature,
    ) -> Result<(), AuthenticationError>;
}

/// Always accepts. Useful in tests; do not use in production.
pub struct ValidateNothing;

impl Authenticator for ValidateNothing {
    fn check(&self, _: CertificateHeader, _: Signature) -> Result<(), AuthenticationError> {
        Ok(())
    }
}

/// Validates the certificate's lifetime against the supplied clock.
/// Does NOT verify the signature.
pub struct ValidateTime<C: Clock> {
    pub clock: C,
}

impl<C: Clock> Authenticator for ValidateTime<C> {
    fn check(&self, hdr: CertificateHeader, _: Signature) -> Result<(), AuthenticationError> {
        check_time(&self.clock, &hdr)
    }
}

/// Validates the certificate's signature against the configured authority
/// public key. Does NOT check time validity.
pub struct ValidateSignature<'a, C: secp256k1::Verification> {
    pub authority: PublicKey,
    pub ctx: &'a secp256k1::Secp256k1<C>,
}

impl<'a, C: secp256k1::Verification> Authenticator for ValidateSignature<'a, C> {
    fn check(&self, hdr: CertificateHeader, sig: Signature) -> Result<(), AuthenticationError> {
        verify_signature(&self.authority, self.ctx, &hdr, &sig)
    }
}

/// Validates both signature and time. The recommended production
/// authenticator.
pub struct ValidateFull<'a, K: Clock, C: secp256k1::Verification> {
    pub authority: PublicKey,
    pub ctx: &'a secp256k1::Secp256k1<C>,
    pub clock: K,
}

impl<'a, K: Clock, C: secp256k1::Verification> Authenticator for ValidateFull<'a, K, C> {
    fn check(&self, hdr: CertificateHeader, sig: Signature) -> Result<(), AuthenticationError> {
        check_time(&self.clock, &hdr)?;
        verify_signature(&self.authority, self.ctx, &hdr, &sig)
    }
}

fn check_time<C: Clock>(clock: &C, hdr: &CertificateHeader) -> Result<(), AuthenticationError> {
    let now = clock.now_unix_secs()?;
    if hdr.valid_from > hdr.not_valid_after {
        return Err(AuthenticationError::BrokenCertificate);
    }
    if now < hdr.valid_from {
        return Err(AuthenticationError::CertificateNotYetValid);
    }
    if now > hdr.not_valid_after {
        return Err(AuthenticationError::CertificateNoMoreValid);
    }
    Ok(())
}

fn verify_signature<C: secp256k1::Verification>(
    authority: &PublicKey,
    ctx: &secp256k1::Secp256k1<C>,
    hdr: &CertificateHeader,
    sig: &Signature,
) -> Result<(), AuthenticationError> {
    let sig_bytes: &[u8; 64] = sig
        .as_ref()
        .try_into()
        .map_err(|_| AuthenticationError::SignatureInvalid)?;
    let schnorr_sig = secp256k1::schnorr::Signature::from_byte_array(*sig_bytes);

    let pk_bytes: &[u8; 32] = authority
        .as_ref()
        .try_into()
        .map_err(|_| AuthenticationError::BadPublicKey)?;
    let xonly_pk = secp256k1::XOnlyPublicKey::from_byte_array(*pk_bytes)
        .map_err(|_| AuthenticationError::BadPublicKey)?;

    let msg = secp256k1::Message::from_digest(hdr.hash());

    ctx.verify_schnorr(&schnorr_sig, msg.as_ref(), &xonly_pk)
        .map_err(|_| AuthenticationError::SignatureInvalid)
}

// ---- Authenticator delegation through references -----------------------

impl<A: Authenticator + ?Sized> Authenticator for &A {
    fn check(
        &self,
        certificate_header: CertificateHeader,
        signature: Signature,
    ) -> Result<(), AuthenticationError> {
        (**self).check(certificate_header, signature)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::certificate::{CertificateSigningRequest, CertificateWithPrivateKey};
    use crate::keys::SecretKey;
    use ii_sv2_crypto::{CryptoCtx, Rng, new_keypair};

    /// Test fixture: returns a fixed unix timestamp (no real clock needed).
    struct FakeClock(u32);
    impl Clock for FakeClock {
        fn now_unix_secs(&self) -> Result<u32, AuthenticationError> {
            Ok(self.0)
        }
    }

    struct MyRng;
    impl Rng for MyRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            use rand::RngCore;
            rand::rng().fill_bytes(dest);
        }
    }

    /// `now` here is "now" as seen by a synthetic clock — no system time.
    fn cert_hdr(now: u32, from_offset: i32, to_offset: i32, rspk: PublicKey) -> CertificateHeader {
        CertificateHeader {
            version: 0,
            valid_from: (now as i32 + from_offset) as u32,
            not_valid_after: (now as i32 + to_offset) as u32,
            remote_static_public_key: rspk,
        }
    }

    #[test]
    fn certificate_hash_is_deterministic() {
        let hdr = CertificateHeader {
            version: 1,
            valid_from: 1_700_000_000,
            not_valid_after: 1_900_000_000,
            remote_static_public_key: PublicKey::new([7; 32]),
        };
        assert_eq!(hdr.hash(), hdr.hash());
        // Mutating any field changes the hash.
        let mut other = hdr;
        other.version = 2;
        assert_ne!(hdr.hash(), other.hash());
    }

    /// Mirrors a certificate-validator conformance test, adapted to use
    /// a synthetic clock.
    #[test]
    fn certificate_validators() {
        const NOW: u32 = 1_700_000_000;
        let ctx = CryptoCtx::new();
        let ca_keypair = new_keypair(&mut MyRng, &ctx);
        let static_kp = new_keypair(&mut MyRng, &ctx);
        let rspk = PublicKey::from(&static_kp);
        let ca_pub = PublicKey::from(&ca_keypair);

        // ---- Currently valid certificate (now-1000 .. now+1000) ----
        let cert = CertificateSigningRequest {
            header: cert_hdr(NOW, -1000, 1000, rspk),
        }
        .sign(&ca_keypair, ctx.as_secp256k1());

        ValidateSignature {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
        }
        .check(cert.header, cert.signature)
        .expect("valid sig");
        ValidateTime {
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .expect("within validity window");
        ValidateFull {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .expect("valid + in-window");

        // ---- Not yet valid (now+100 .. now+1000) ----
        let cert = CertificateSigningRequest {
            header: cert_hdr(NOW, 100, 1000, rspk),
        }
        .sign(&ca_keypair, ctx.as_secp256k1());

        ValidateSignature {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
        }
        .check(cert.header, cert.signature)
        .expect("sig still valid");
        let err = ValidateTime {
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::CertificateNotYetValid);
        let err = ValidateFull {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::CertificateNotYetValid);

        // ---- Expired (now-1000 .. now-100) ----
        let cert = CertificateSigningRequest {
            header: cert_hdr(NOW, -1000, -100, rspk),
        }
        .sign(&ca_keypair, ctx.as_secp256k1());

        ValidateSignature {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
        }
        .check(cert.header, cert.signature)
        .expect("sig still valid");
        let err = ValidateTime {
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::CertificateNoMoreValid);
        let err = ValidateFull {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::CertificateNoMoreValid);

        // ---- Forged expiry date: signature breaks ----
        let mut cert = CertificateSigningRequest {
            header: cert_hdr(NOW, -1000, 1000, rspk),
        }
        .sign(&ca_keypair, ctx.as_secp256k1());
        cert.header.not_valid_after = 3_155_759_999; // far future
        let err = ValidateSignature {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::SignatureInvalid);
        // Time alone now passes (cert's own "to" is far future, so still in window):
        ValidateTime {
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .expect("time alone passes");
        // Full fails on the signature.
        let err = ValidateFull {
            authority: ca_pub,
            ctx: ctx.as_secp256k1(),
            clock: FakeClock(NOW),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::SignatureInvalid);

        // ---- Forged authority: signature fails against the wrong pubkey ----
        let forged_authority = new_keypair(&mut MyRng, &ctx);
        let cert = CertificateSigningRequest {
            header: cert_hdr(NOW, -1000, 1000, rspk),
        }
        .sign(&ca_keypair, ctx.as_secp256k1());
        let err = ValidateSignature {
            authority: PublicKey::from(&forged_authority),
            ctx: ctx.as_secp256k1(),
        }
        .check(cert.header, cert.signature)
        .unwrap_err();
        assert_eq!(err, AuthenticationError::SignatureInvalid);
    }

    #[test]
    fn cert_with_private_key_to_certificate() {
        let ctx = CryptoCtx::new();
        let ca = new_keypair(&mut MyRng, &ctx);
        let server_kp = new_keypair(&mut MyRng, &ctx);
        let header = CertificateHeader {
            version: 0,
            valid_from: 1,
            not_valid_after: 2,
            remote_static_public_key: PublicKey::from(&server_kp),
        };
        let cert = CertificateSigningRequest { header }.sign(&ca, ctx.as_secp256k1());
        let bundle = CertificateWithPrivateKey {
            header: cert.header,
            authority_public_key: cert.authority_public_key,
            server_static_private_key: SecretKey::from(&server_kp),
            signature: cert.signature,
        };
        let stripped: crate::certificate::Certificate = bundle.into();
        assert_eq!(stripped.header, cert.header);
        assert_eq!(stripped.authority_public_key, cert.authority_public_key);
        assert_eq!(stripped.signature, cert.signature);
    }

    #[test]
    fn validate_nothing_accepts_anything() {
        let hdr = CertificateHeader {
            version: 99,
            valid_from: 0,
            not_valid_after: 0,
            remote_static_public_key: PublicKey::new([0; 32]),
        };
        let sig = Signature::new([0; 64]);
        ValidateNothing.check(hdr, sig).unwrap();
    }
}
