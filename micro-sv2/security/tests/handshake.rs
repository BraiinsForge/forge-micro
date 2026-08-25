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
//!
//! Reads as documentation. The lifecycle is:
//!
//!   1. Implement [`Rng`] and a [`Clock`]; build a [`CryptoCtx`].
//!   2. Generate the authority (CA) keypair.
//!   3. Generate the server's static keypair and issue a certificate.
//!   4. Run [`initiate`] and [`accept`] concurrently over an
//!      `embedded_io_async`-flavored IO pair.
//!   5. Use the resulting [`EstablishedSession::transport`] for
//!      application-level traffic.

use embedded_io_adapters::tokio_1::FromTokio;
use ii_sv2_crypto::{CryptoCtx, Rng, Secp256k1KeyPair};
use ii_sv2_security::{
    AuthenticationError, CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey,
    Clock, PublicKey, SecretKey, ValidateFull, accept, initiate,
};

/// Step 1a: an `Rng` impl. On hardware this would wrap the platform's
/// secure RNG; here we use `rand` because tests run on the host.
struct TestRng;

impl Rng for TestRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        use rand::RngCore;
        rand::rng().fill_bytes(dest);
    }
}

/// Step 1b: a `Clock` impl. On hardware this would read the platform
/// RTC and convert to seconds since the unix epoch.
struct FakeClock(u32);

impl Clock for FakeClock {
    fn now_unix_secs(&self) -> Result<u32, AuthenticationError> {
        Ok(self.0)
    }
}

#[tokio::test]
async fn full_handshake_with_validated_certificate() {
    // The clock our authenticator will trust. In real deployments this
    // is the host's RTC.
    const NOW: u32 = 1_700_000_000;

    // Step 2: generate the authority (CA) keypair. The CA's public key
    // is what the client trusts ahead of time and uses to validate
    // the server's certificate signature.
    let ctx = CryptoCtx::new();
    let authority_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);
    let authority_pk = PublicKey::from(&authority_kp);

    // Step 3a: generate the server's static keypair. This is the
    // long-term identity bound into the certificate.
    let server_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);

    // Step 3b: assemble the certificate header (validity window plus
    // the server's pubkey) and have the authority sign it.
    let header = CertificateHeader {
        version: 0,
        valid_from: NOW - 60 * 60,           // valid since 1h ago
        not_valid_after: NOW + 24 * 60 * 60, // expires in 24h
        remote_static_public_key: PublicKey::from(&server_kp),
    };
    let signed = CertificateSigningRequest { header }.sign(&authority_kp, ctx.as_secp256k1());

    // Step 3c: bundle the signed certificate with the server's secret
    // key. This is what the responder needs to accept connections.
    let server_bundle = CertificateWithPrivateKey {
        header: signed.header,
        authority_public_key: signed.authority_public_key,
        server_static_private_key: SecretKey::from(&server_kp),
        signature: signed.signature,
    };

    // Step 4: set up an in-memory loopback. tokio::io::duplex gives us
    // two ends; we wrap each in FromTokio to satisfy the
    // embedded_io_async::Read + Write bounds on initiate / accept.
    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut client_io = FromTokio::new(client_io);
    let mut server_io = FromTokio::new(server_io);

    // Step 5a: build the client's authenticator. ValidateFull verifies
    // both the certificate signature against `authority_pk` and the
    // validity window against the supplied clock.
    let authenticator = ValidateFull {
        authority: authority_pk,
        ctx: ctx.as_secp256k1(),
        clock: FakeClock(NOW),
    };

    // Step 5b: drive both sides concurrently. In production the client
    // and server are separate processes; here they share a runtime.
    let (client_result, server_result) = tokio::join!(
        initiate(&mut client_io, TestRng, &ctx, authenticator),
        accept(&mut server_io, TestRng, &ctx, &server_bundle),
    );

    let mut client = client_result.expect("client handshake");
    let mut server = server_result.expect("server handshake");

    // Step 6: cross-check the session.
    // Both sides agree on the session id (derived from the noise
    // handshake hash).
    assert_eq!(client.session_id, server.session_id);
    // The client now knows the server's static key and has validated
    // the certificate it came with.
    assert_eq!(client.remote_identity, Some(PublicKey::from(&server_kp)));
    // NX does not authenticate the initiator; the server learns no
    // initiator identity.
    assert_eq!(server.remote_identity, None);

    // Step 7: exchange application data through the noise transport.
    // Encrypts in place; the buffer must hold ciphertext + MAC.
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

    // ... and a reply going the other direction.
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

/// Sanity: an expired certificate should make the client reject the
/// handshake. Same setup as above but the cert is expired by the time
/// the clock reads `NOW`.
#[tokio::test]
async fn expired_certificate_is_rejected() {
    const NOW: u32 = 1_700_000_000;

    let ctx = CryptoCtx::new();
    let authority_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);
    let server_kp = Secp256k1KeyPair::generate(&mut TestRng, &ctx);

    // Cert that expired one hour before NOW.
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
