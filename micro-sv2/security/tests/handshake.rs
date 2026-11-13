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

//! End-to-end demonstration of the SV2 noise handshake driven by
//! `ii-sv2-security`.

use embedded_io_adapters::tokio_1::FromTokio;
use ii_sv2_crypto::{CryptoCtx, Rng, Secp256k1KeyPair};
use ii_sv2_security::{
    AuthenticationError, CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey,
    Clock, PublicKey, SecretKey, ValidateFull, accept, initiate,
};

struct TestRng;

impl Rng for TestRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        use rand::Rng as _;
        rand::rng().fill_bytes(dest);
    }
}

struct FakeClock(u32);

impl Clock for FakeClock {
    fn now_unix_secs(&self) -> Result<u32, AuthenticationError> {
        Ok(self.0)
    }
}

#[tokio::test]
async fn full_handshake_with_validated_certificate() {
    const NOW: u32 = 1_700_000_000;

    // The client trusts the authority public key ahead of time and
    // validates the server certificate against it.
    let ctx = CryptoCtx::new();
    let authority_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);
    let authority_pk = PublicKey::from(&authority_kp);

    // The server's long-term identity, bound into the certificate.
    let server_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);

    let header = CertificateHeader {
        version: 0,
        valid_from: NOW - 60 * 60,           // valid since 1h ago
        not_valid_after: NOW + 24 * 60 * 60, // expires in 24h
        remote_static_public_key: PublicKey::from(&server_kp),
    };
    let signed = CertificateSigningRequest { header }.sign(&authority_kp, ctx.as_secp256k1());

    let server_bundle = CertificateWithPrivateKey {
        header: signed.header,
        authority_public_key: signed.authority_public_key,
        server_static_private_key: SecretKey::from(&server_kp),
        signature: signed.signature,
    };

    // FromTokio satisfies the `embedded_io_async` bounds on initiate
    // and accept.
    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut client_io = FromTokio::new(client_io);
    let mut server_io = FromTokio::new(server_io);

    let authenticator = ValidateFull {
        authority: authority_pk,
        ctx: ctx.as_secp256k1(),
        clock: FakeClock(NOW),
    };

    let (client_result, server_result) = tokio::join!(
        initiate(&mut client_io, TestRng, &ctx, authenticator),
        accept(&mut server_io, TestRng, &ctx, &server_bundle),
    );

    let mut client = client_result.expect("client handshake");
    let mut server = server_result.expect("server handshake");

    assert_eq!(client.session_id, server.session_id);
    assert_eq!(client.remote_identity, Some(PublicKey::from(&server_kp)));
    // NX does not authenticate the initiator.
    assert_eq!(server.remote_identity, None);

    // Encryption is in place, so the buffer holds ciphertext and MAC.
    let request = b"GET /work HTTP/1.0";
    let mut buf = [0u8; 256];
    buf[..request.len()].copy_from_slice(request);
    let ct_len = client
        .transport
        .write_message_inplace(&mut buf, request.len())
        .expect("client seal");
    let pt_len = server
        .transport
        .read_message_inplace(&mut buf[..ct_len])
        .expect("server open");
    assert_eq!(&buf[..pt_len], request);

    // ... and a reply the other way.
    let reply = b"200 OK";
    let mut ct_buf = [0u8; 256];
    let ct_len = server
        .transport
        .write_message(reply, &mut ct_buf)
        .expect("server seal");
    let mut pt_buf = [0u8; 256];
    let pt_len = client
        .transport
        .read_message(&ct_buf[..ct_len], &mut pt_buf)
        .expect("client open");
    assert_eq!(&pt_buf[..pt_len], reply);
}

#[tokio::test]
async fn expired_certificate_is_rejected() {
    const NOW: u32 = 1_700_000_000;

    let ctx = CryptoCtx::new();
    let authority_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);
    let server_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);

    let header = CertificateHeader {
        version: 0,
        valid_from: NOW - 24 * 60 * 60,
        not_valid_after: NOW - 60 * 60,
        remote_static_public_key: PublicKey::from(&server_kp),
    };
    let signed = CertificateSigningRequest { header }.sign(&authority_kp, ctx.as_secp256k1());
    let server_bundle = CertificateWithPrivateKey {
        header: signed.header,
        authority_public_key: signed.authority_public_key,
        server_static_private_key: SecretKey::from(&server_kp),
        signature: signed.signature,
    };

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut client_io = FromTokio::new(client_io);
    let mut server_io = FromTokio::new(server_io);

    let authenticator = ValidateFull {
        authority: PublicKey::from(&authority_kp),
        ctx: ctx.as_secp256k1(),
        clock: FakeClock(NOW),
    };

    let (client_result, _) = tokio::join!(
        initiate(&mut client_io, TestRng, &ctx, authenticator),
        accept(&mut server_io, TestRng, &ctx, &server_bundle),
    );

    use ii_sv2_security::{AuthenticationError as AE, InitializationError};
    match client_result {
        Err(InitializationError::Authentication(AE::CertificateNoMoreValid)) => {}
        Err(other) => panic!("expected CertificateNoMoreValid, got error {:?}", other),
        Ok(_) => panic!("expected handshake failure, got success"),
    }
}
