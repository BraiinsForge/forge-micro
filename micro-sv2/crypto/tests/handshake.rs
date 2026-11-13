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

//! End-to-end demonstration of the SV2 Noise NX handshake.

use ii_sv2_crypto::{
    CryptoCtx, EPHEMERAL_KEY_MSG_SIZE, HandShake, MAC_SIZE, Rng, STATIC_KEY_MSG_SIZE,
    keys::Secp256k1KeyPair,
};

struct MyRng;

impl Rng for MyRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        use rand::Rng as _;
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

    let ctx = CryptoCtx::new();
    let static_kp = Secp256k1KeyPair::generate(&mut MyRng, &ctx);
    let expected_remote_pk = static_kp.xonly_public_key_bytes();

    // In NX only the responder holds a static key.
    let mut initiator = HandShake::new_initiator(MyRng, &ctx);
    let mut responder = HandShake::new_responder(MyRng, &ctx, static_kp);

    let mut wire = [0u8; HANDSHAKE_BUF_LEN];
    let mut scratch = [0u8; HANDSHAKE_BUF_LEN];

    let m1 = initiator.write_message(b"", &mut wire).expect("write -> e");
    assert_eq!(m1, EPHEMERAL_KEY_MSG_SIZE);

    // `-> e` carries no payload, so the second buffer is scratch.
    let m2 = responder
        .read_message(&wire[..m1], &mut scratch)
        .expect("read -> e");
    assert_eq!(m2, 0);

    // `<- e, ee, s, es` carries an authenticated payload, which the
    // protocol uses for the responder's certificate.
    let cert = [0xABu8; CERT_LEN];
    let m3 = responder
        .write_message(&cert, &mut wire)
        .expect("write <- e, ee, s, es, cert");

    // Decryption is in place, so the read buffer holds ciphertext and
    // MAC rather than only the plaintext.
    let mut received_cert = [0u8; CERT_LEN + MAC_SIZE];
    let m4 = initiator
        .read_message(&wire[..m3], &mut received_cert)
        .expect("read <- e, ee, s, es, cert");
    assert_eq!(m4, CERT_LEN);
    assert_eq!(&received_cert[..m4], &cert[..]);

    let mut i_transport = initiator.finish().expect("initiator handshake complete");
    let mut r_transport = responder.finish().expect("responder handshake complete");

    assert_eq!(i_transport.handshake_hash(), r_transport.handshake_hash());

    assert_eq!(i_transport.remote_static_pubkey(), Some(expected_remote_pk));
    // NX does not authenticate the initiator.
    assert_eq!(r_transport.remote_static_pubkey(), None);

    // Application data through the in-place AEAD API.
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

    // ... and through the copying API.
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
