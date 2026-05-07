//! End-to-end demonstration of the SV2 Noise NX handshake.
//!
//! This file doubles as the canonical "how do I use this crate" example.
//! It walks through the four-step lifecycle:
//!
//!   1. Implement [`Rng`] and build a [`CryptoCtx`] (the long-lived
//!      secp256k1 precomputation context).
//!   2. Generate the responder's static keypair (this authenticates the
//!      responder).
//!   3. Drive the handshake by alternating `write_message` / `read_message`
//!      on each side until `finish()` succeeds.
//!   4. Use the resulting [`TransportState`] to seal / open application
//!      messages.

use ii_sv2_crypto::{
    CryptoCtx, EPHEMERAL_KEY_MSG_SIZE, HandShake, MAC_SIZE, Rng, STATIC_KEY_MSG_SIZE,
    keys::new_keypair,
};

/// Step 1: a minimal `Rng` for tests. On real hardware this would wrap
/// the platform's secure RNG; the secp256k1 context is supplied
/// separately as a [`CryptoCtx`] (e.g. backed by
/// `secp256k1::AllPreallocated<'static>` over a `static mut [u8; N]`).
struct MyRng;

impl Rng for MyRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        use rand::RngCore;
        rand::rng().fill_bytes(dest);
    }
}

#[test]
fn nx_handshake_end_to_end() {
    // The largest handshake message is the responder's reply:
    //   e || enc(s) || enc(cert + MAC)
    const CERT_LEN: usize = 64;
    const HANDSHAKE_BUF_LEN: usize =
        EPHEMERAL_KEY_MSG_SIZE + STATIC_KEY_MSG_SIZE + CERT_LEN + MAC_SIZE;

    // Step 2: generate the responder's long-term static keypair. Its public
    // half is what the initiator will recover from the handshake.
    let ctx = CryptoCtx::new();
    let static_kp = new_keypair(&mut MyRng, &ctx);
    let expected_remote_pk = static_kp.xonly_public_key_bytes();

    // Step 3: set up both sides. The initiator does not have a static key
    // in NX — only the responder does.
    let mut initiator = HandShake::new_initiator(MyRng, &ctx);
    let mut responder = HandShake::new_responder(MyRng, &ctx, static_kp);

    // Wire buffer; in production these bytes travel over a TCP stream.
    let mut wire = [0u8; HANDSHAKE_BUF_LEN];
    let mut scratch = [0u8; HANDSHAKE_BUF_LEN];

    // i -> r: `-> e` (just the initiator's ephemeral key)
    let m1 = initiator.write_message(b"", &mut wire).expect("write -> e");
    assert_eq!(m1, EPHEMERAL_KEY_MSG_SIZE);

    // r reads `e`. There's no payload for the responder to extract here;
    // the second buffer is just scratch space.
    let m2 = responder
        .read_message(&wire[..m1], &mut scratch)
        .expect("read -> e");
    assert_eq!(m2, 0);

    // r -> i: `<- e, ee, s, es` plus an authenticated payload (commonly the
    // server's certificate / signature blob).
    let cert = [0xABu8; CERT_LEN];
    let m3 = responder
        .write_message(&cert, &mut wire)
        .expect("write <- e, ee, s, es, cert");

    // i reads the response and recovers the cert plaintext.
    // NB: the read buffer must hold ciphertext+MAC during in-place
    // decryption — size it to `CERT_LEN + MAC_SIZE`, not `CERT_LEN`.
    let mut received_cert = [0u8; CERT_LEN + MAC_SIZE];
    let m4 = initiator
        .read_message(&wire[..m3], &mut received_cert)
        .expect("read <- e, ee, s, es, cert");
    assert_eq!(m4, CERT_LEN);
    assert_eq!(&received_cert[..m4], &cert[..]);

    // Step 4: both sides finish() into a TransportState.
    let mut i_transport = initiator.finish().expect("initiator handshake complete");
    let mut r_transport = responder.finish().expect("responder handshake complete");

    // The handshake hash agrees on both sides — this is what application
    // protocols (e.g. cert binding) sign over.
    assert_eq!(i_transport.handshake_hash(), r_transport.handshake_hash());

    // The initiator now knows the responder's static public key.
    assert_eq!(i_transport.remote_static_pubkey(), Some(expected_remote_pk));
    // The responder does not learn an initiator static key in NX.
    assert_eq!(r_transport.remote_static_pubkey(), None);

    // Application data: i -> r using the in-place AEAD API.
    let plaintext = b"hello from initiator";
    let mut buf = [0u8; 256];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct_len = i_transport
        .write_message_inplace(&mut buf, plaintext.len())
        .expect("seal i -> r");
    let pt_len = r_transport
        .read_message_inplace(&mut buf[..ct_len])
        .expect("open at r");
    assert_eq!(&buf[..pt_len], plaintext);

    // Application data: r -> i using the copying API.
    let reply = b"ack from responder";
    let mut ct_buf = [0u8; 256];
    let ct_len = r_transport
        .write_message(reply, &mut ct_buf)
        .expect("seal r -> i");
    let mut pt_buf = [0u8; 256];
    let pt_len = i_transport
        .read_message(&ct_buf[..ct_len], &mut pt_buf)
        .expect("open at i");
    assert_eq!(&pt_buf[..pt_len], reply);
}
