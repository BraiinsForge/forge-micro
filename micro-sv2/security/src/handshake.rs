//! Async, runtime-agnostic Stratum V2 noise handshake driver.
//!
//! Generic over [`embedded_io_async::Read`] + [`embedded_io_async::Write`],
//! so the same code runs under embassy, smol, or — via
//! `embedded-io-adapters` — tokio. No runtime is a dependency of this
//! crate; the caller's executor polls our `Future`.
//!
//! Both [`initiate`] and [`accept`] take an [`Rng`] for fresh entropy
//! and a borrowed [`CryptoCtx`] for the long-lived secp256k1
//! precomputation context. After the handshake completes, the
//! [`EstablishedSession`] holds the noise [`TransportState`] you'll use
//! for application traffic.

use embedded_io_async::{Read, Write};
use ii_sv2_crypto::{
    CryptoCtx, EPHEMERAL_KEY_MSG_SIZE, HandShake, NXError, Rng, STATIC_KEY_MSG_SIZE,
    Secp256k1KeyPair, SecretSecp256k1Key, TransportState,
};

use crate::authenticator::Authenticator;
use crate::certificate::{
    Certificate, CertificateWithPrivateKey, NOISE_SIG_MSG_SIZE, NOISE_SIG_SIZE,
};
use crate::error::{InitializationError, KeyParseError};
use crate::keys::{PublicKey, SessionId};

/// What a successful handshake leaves you with: a noise transport, the
/// peer's authenticated identity (initiator only), and the session
/// identifier derived from the handshake hash.
pub struct EstablishedSession {
    pub transport: TransportState,
    /// Static public key the peer authenticated with via the
    /// certificate's noise signature message. `None` for responders —
    /// NX does not authenticate the initiator.
    pub remote_identity: Option<PublicKey>,
    pub session_id: SessionId,
}

const HANDSHAKE_BUF_LEN: usize =
    EPHEMERAL_KEY_MSG_SIZE + STATIC_KEY_MSG_SIZE + NOISE_SIG_MSG_SIZE;

/// Drive the initiator side of the SV2 noise handshake over `io`.
///
/// 1. Sends `-> e`.
/// 2. Reads `<- e, ee, s, es, [signed_cert_msg]`.
/// 3. Parses the responder's certificate from the encrypted payload and
///    runs `authenticator.check(...)`.
pub async fn initiate<T, R, A, C>(
    io: &mut T,
    rng: R,
    ctx: &CryptoCtx<C>,
    authenticator: A,
) -> Result<EstablishedSession, InitializationError<T::Error>>
where
    T: Read + Write,
    R: Rng,
    A: Authenticator,
    C: secp256k1::Signing + secp256k1::Verification,
{
    let mut handshake = HandShake::new_initiator(rng, ctx);
    let mut buf = [0u8; HANDSHAKE_BUF_LEN];

    // -> e
    let m1_len = handshake.write_message(&[], &mut buf)?;
    debug_assert_eq!(m1_len, EPHEMERAL_KEY_MSG_SIZE);
    io.write_all(&buf[..EPHEMERAL_KEY_MSG_SIZE])
        .await
        .map_err(InitializationError::Io)?;

    // <- e, ee, s, es, [signed_cert_msg]
    io.read_exact(&mut buf).await?;
    let mut sig_msg_buf = [0u8; NOISE_SIG_MSG_SIZE];
    let sig_pt_len = handshake.read_message(&buf, &mut sig_msg_buf)?;

    let transport = handshake.finish()?;
    let remote_static_key = transport
        .remote_static_pubkey()
        .ok_or(InitializationError::Handshake(NXError::InvalidState))?;

    // Authenticate the peer's certificate.
    let (cert_hdr, signature) =
        Certificate::parse_noise_signature_msg(remote_static_key, &sig_msg_buf[..sig_pt_len]);
    let remote_identity = cert_hdr.remote_static_public_key;
    authenticator.check(cert_hdr, signature)?;

    let session_id = SessionId(transport.handshake_hash());
    Ok(EstablishedSession {
        transport,
        remote_identity: Some(remote_identity),
        session_id,
    })
}

/// Drive the responder side of the SV2 noise handshake over `io`.
///
/// 1. Reads `-> e`.
/// 2. Reconstructs the static keypair from `cert.server_static_private_key`,
///    mixing fresh entropy from `rng` for the elligator encoding.
/// 3. Sends `<- e, ee, s, es` together with the noise signature message
///    derived from the (public) certificate.
pub async fn accept<T, R, C>(
    io: &mut T,
    mut rng: R,
    ctx: &CryptoCtx<C>,
    cert: &CertificateWithPrivateKey,
) -> Result<EstablishedSession, InitializationError<T::Error>>
where
    T: Read + Write,
    R: Rng,
    C: secp256k1::Signing + secp256k1::Verification,
{
    let secret_bytes: [u8; 32] = cert.server_static_private_key.into();
    let secp_secret = SecretSecp256k1Key::try_from(secret_bytes)
        .map_err(|_| InitializationError::InvalidKey(KeyParseError::InvalidKey))?;
    let server_kp = Secp256k1KeyPair::from_secret_key(&secp_secret, &mut rng, ctx);

    let mut handshake = HandShake::new_responder(rng, ctx, server_kp);
    let mut buf = [0u8; HANDSHAKE_BUF_LEN];

    // -> e
    io.read_exact(&mut buf[..EPHEMERAL_KEY_MSG_SIZE]).await?;
    handshake.read_message(&buf[..EPHEMERAL_KEY_MSG_SIZE], &mut [])?;

    // <- e, ee, s, es, [signed_cert_msg]
    let mut sig_msg_buf = [0u8; NOISE_SIG_SIZE];
    let public_cert = Certificate::from(*cert);
    public_cert.serialize_noise_signature_msg(&mut sig_msg_buf);
    let m_len = handshake.write_message(&sig_msg_buf, &mut buf)?;
    debug_assert_eq!(m_len, HANDSHAKE_BUF_LEN);
    io.write_all(&buf)
        .await
        .map_err(InitializationError::Io)?;

    let transport = handshake.finish()?;
    let session_id = SessionId(transport.handshake_hash());
    Ok(EstablishedSession {
        transport,
        remote_identity: None,
        session_id,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::authenticator::ValidateNothing;
    use crate::certificate::{CertificateHeader, CertificateSigningRequest};
    use crate::keys::SecretKey;
    use embedded_io_adapters::tokio_1::FromTokio;
    use ii_sv2_crypto::{CryptoCtx, Rng, new_keypair};

    struct MyRng;
    impl Rng for MyRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            use rand::RngCore;
            rand::rng().fill_bytes(dest);
        }
    }

    /// Loopback handshake over an in-memory `tokio::io::duplex` pipe,
    /// wrapped to satisfy `embedded_io_async`. Confirms initiate +
    /// accept agree, the initiator recovers the responder's static key,
    /// and the resulting transport states encrypt and decrypt symmetrically.
    #[tokio::test]
    async fn handshake_loopback() {
        let (a, b) = tokio::io::duplex(8192);
        let mut a = FromTokio::new(a);
        let mut b = FromTokio::new(b);

        // Issue a self-signed certificate for the responder.
        let ctx = CryptoCtx::new();
        let server_kp = new_keypair(&mut MyRng, &ctx);
        let authority_kp = new_keypair(&mut MyRng, &ctx);
        let header = CertificateHeader {
            version: 0,
            valid_from: 0,
            not_valid_after: u32::MAX,
            remote_static_public_key: PublicKey::from(&server_kp),
        };
        let cert = CertificateSigningRequest { header }.sign(&authority_kp, ctx.as_secp256k1());
        let bundle = CertificateWithPrivateKey {
            header: cert.header,
            authority_public_key: cert.authority_public_key,
            server_static_private_key: SecretKey::from(&server_kp),
            signature: cert.signature,
        };

        let (init_result, accept_result) = tokio::join!(
            initiate(&mut a, MyRng, &ctx, ValidateNothing),
            accept(&mut b, MyRng, &ctx, &bundle),
        );

        let mut i_session = init_result.expect("initiator handshake");
        let mut r_session = accept_result.expect("responder handshake");

        assert_eq!(i_session.session_id, r_session.session_id);
        assert_eq!(
            i_session.remote_identity,
            Some(PublicKey::from(&server_kp))
        );
        assert_eq!(r_session.remote_identity, None);

        // The transport states should encrypt/decrypt symmetrically.
        let plaintext = b"hello over noise";
        let mut buf = [0u8; 64];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct_len = i_session
            .transport
            .write_message_inplace(&mut buf, plaintext.len())
            .expect("seal");
        let pt_len = r_session
            .transport
            .read_message_inplace(&mut buf[..ct_len])
            .expect("open");
        assert_eq!(&buf[..pt_len], plaintext);
    }
}
