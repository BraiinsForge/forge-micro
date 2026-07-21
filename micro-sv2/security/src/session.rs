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

//! `EncryptedCodec` — a [`FrameCodec`] over [`ii_sv2_crypto::TransportState`].
//!
//! Hides the two-AEAD-chunks-per-frame wire layout behind the trait, so
//! callers send and receive Stratum V2 frames without thinking about how
//! the bytes are protected.
//!
//! # Wire layout per frame
//!
//! ```text
//! ┌──────────────┬─────────┬───────────────────┬─────────┐
//! │ enc(Header)  │ MAC(16) │ enc(payload..N..) │ MAC(16) │
//! └──────────────┴─────────┴───────────────────┴─────────┘
//!  6 bytes plain            N = header.msg_length
//! ```
//!
//! # Single-chunk MVP
//!
//! `encode` and `decode_frame` seal/open the payload as a *single* AEAD
//! chunk, which is correct for `body_len <= 65 519` (every elementary
//! mining message with margin to spare). Larger payloads return
//! [`SessionError::ChunkingNotImplemented`] explicitly. The multi-chunk
//! loop (mirroring `stratum2-noise/src/codec.rs`) is a future
//! non-breaking enhancement.

use ii_sv2_crypto::{AeadError, MAC_SIZE, TransportState};
use ii_sv2_framing::{FrameCodec, FrameView, FramingError, Header, IntoFrame};
use ii_sv2_types::{Decode, Encode};

use crate::handshake::EstablishedSession;
use crate::keys::SessionId;

/// Largest plaintext that fits in a single SV2 noise frame (65535 - 16).
const MAX_PLAINTEXT_LENGTH: usize = 0xffff - MAC_SIZE;

#[derive(Debug)]
#[non_exhaustive]
pub enum SessionError {
    /// Caller-supplied buffer too small for the operation.
    BufferTooSmall,
    /// AEAD seal/open failed.
    Crypto(AeadError),
    /// Header parse or other framing-level failure.
    Framing(FramingError),
    /// Body length exceeds what the single-chunk MVP can handle.
    /// Multi-chunk noise framing is not yet implemented.
    ChunkingNotImplemented { length: usize },
}

impl From<AeadError> for SessionError {
    fn from(e: AeadError) -> Self {
        Self::Crypto(e)
    }
}
impl From<FramingError> for SessionError {
    fn from(e: FramingError) -> Self {
        Self::Framing(e)
    }
}

/// Stateful frame codec that protects every frame with two AEAD chunks
/// (header chunk and body chunk).
pub struct EncryptedCodec {
    transport: TransportState,
}

impl EncryptedCodec {
    /// Wrap a [`TransportState`] directly. Most callers should use
    /// [`from_handshake`](Self::from_handshake) instead.
    pub fn from_transport(transport: TransportState) -> Self {
        Self { transport }
    }

    /// Build from the [`EstablishedSession`] returned by
    /// [`crate::handshake::initiate`] / [`crate::handshake::accept`].
    /// Returns the session id alongside so callers can keep it.
    pub fn from_handshake(session: EstablishedSession) -> (Self, SessionId) {
        (
            Self {
                transport: session.transport,
            },
            session.session_id,
        )
    }
}

impl FrameCodec for EncryptedCodec {
    type Error = SessionError;
    const HEADER_WIRE_SIZE: usize = Header::SIZE + MAC_SIZE; // 22

    fn payload_wire_size(body_len: usize) -> usize {
        // Single-chunk MVP: ciphertext = plaintext + MAC (or 0 for empty).
        // Only correct for `body_len <= MAX_PLAINTEXT_LENGTH`. The encode
        // path rejects larger plaintext upfront with
        // `ChunkingNotImplemented`. On the receive side, a chunking
        // sender with `header.msg_length > 65519` would cause us to
        // request the wrong byte count; AEAD-open would then fail with
        // `SessionError::Crypto(_)` — not silent corruption, but not
        // graceful interop either. Multi-chunk math is a follow-up.
        if body_len == 0 {
            0
        } else {
            body_len + MAC_SIZE
        }
    }

    fn encode<F: IntoFrame>(&mut self, frame: &F, out: &mut [u8]) -> Result<usize, Self::Error> {
        let body_len = frame.body_len();
        if body_len > MAX_PLAINTEXT_LENGTH {
            return Err(SessionError::ChunkingNotImplemented { length: body_len });
        }
        let total = Self::HEADER_WIRE_SIZE + Self::payload_wire_size(body_len);
        if out.len() < total {
            return Err(SessionError::BufferTooSmall);
        }

        // 1. Plaintext header into out[..6].
        let header = frame.header();
        header
            .encode_to(&mut out[..Header::SIZE])
            .map_err(|e| SessionError::Framing(e.into()))?;

        // 2. AEAD-seal out[..22] in place; MAC lands in [6..22].
        self.transport
            .write_message_inplace(&mut out[..Self::HEADER_WIRE_SIZE], Header::SIZE)?;

        // 3. Body slot via IntoFrame::write_body into out[22..22+body_len].
        let body_start = Self::HEADER_WIRE_SIZE;
        let body_end = body_start + body_len;
        frame.write_body(&mut out[body_start..body_end])?;

        // 4. AEAD-seal out[22..22+body_len+MAC_SIZE] in place; MAC after the body.
        self.transport
            .write_message_inplace(&mut out[body_start..body_end + MAC_SIZE], body_len)?;

        Ok(total)
    }

    fn decode_header(&mut self, buf: &mut [u8]) -> Result<Header, Self::Error> {
        if buf.len() < Self::HEADER_WIRE_SIZE {
            return Err(SessionError::BufferTooSmall);
        }
        let plain_len = self
            .transport
            .read_message_inplace(&mut buf[..Self::HEADER_WIRE_SIZE])?;
        debug_assert_eq!(plain_len, Header::SIZE);
        let mut cursor: &[u8] = &buf[..Header::SIZE];
        Header::decode(&mut cursor).map_err(|e| SessionError::Framing(e.into()))
    }

    fn decode_frame<'a>(
        &mut self,
        header: Header,
        buf: &'a mut [u8],
    ) -> Result<FrameView<'a>, Self::Error> {
        let total = header.msg_length as usize + MAC_SIZE;
        if buf.len() < total {
            return Err(SessionError::BufferTooSmall);
        }
        let plain_len = self.transport.read_message_inplace(&mut buf[..total])?;
        debug_assert_eq!(plain_len, header.msg_length as usize);
        FrameView::try_new(header, &buf[..header.msg_length as usize])
            .map_err(SessionError::Framing)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::authenticator::ValidateNothing;
    use crate::certificate::{
        CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey,
    };
    use crate::handshake::{accept, initiate};
    use crate::keys::{PublicKey, SecretKey};
    use embedded_io_adapters::tokio_1::FromTokio;
    use ii_sv2_crypto::{CryptoCtx, Rng, new_keypair};
    use ii_sv2_framing::{FrameCodec, frame_to_message};
    use ii_sv2_messages::{DeviceInfo, SetupConnection};
    use ii_sv2_types::Str0_255;

    /// Same `MyRng` shape as in `handshake.rs` — host-OS entropy.
    struct MyRng;
    impl Rng for MyRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            use rand::RngCore;
            rand::rng().fill_bytes(dest);
        }
    }

    /// Run a loopback handshake over an in-memory tokio duplex pipe and
    /// return paired (initiator, responder) `EncryptedCodec`s.
    async fn paired_codecs() -> (EncryptedCodec, EncryptedCodec) {
        let (a, b) = tokio::io::duplex(8192);
        let mut a = FromTokio::new(a);
        let mut b = FromTokio::new(b);

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

        let i_session = init_result.expect("initiator handshake");
        let r_session = accept_result.expect("responder handshake");

        let (i_codec, _i_id) = EncryptedCodec::from_handshake(i_session);
        let (r_codec, _r_id) = EncryptedCodec::from_handshake(r_session);
        (i_codec, r_codec)
    }

    /// Smoke test: the fixture builds two paired codecs without panicking.
    #[tokio::test]
    async fn paired_codecs_fixture_builds() {
        let (_i, _r) = paired_codecs().await;
    }

    #[tokio::test]
    async fn roundtrip_single_message() {
        let (mut i_codec, mut r_codec) = paired_codecs().await;

        let outbound = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: Str0_255::try_from("pool.example.com").unwrap(),
            endpoint_port: 3333,
            device: DeviceInfo {
                vendor: Str0_255::try_from("Bitmain").unwrap(),
                hw_rev: Str0_255::try_from("S9i").unwrap(),
                fw_ver: Str0_255::try_from("bos-2018").unwrap(),
                dev_id: Str0_255::try_from("d1").unwrap(),
            },
        };

        // Initiator side: encode the message into a wire buffer.
        let mut wire = [0u8; 1024];
        let n = i_codec.encode(&outbound, &mut wire).expect("encode");

        // Responder side: receive HEADER_WIRE_SIZE bytes, decode header,
        // receive payload bytes, decode frame.
        let mut hbuf = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..EncryptedCodec::HEADER_WIRE_SIZE]);
        let header = r_codec.decode_header(&mut hbuf).expect("decode_header");

        let pn = EncryptedCodec::payload_wire_size(header.msg_length as usize);
        assert_eq!(EncryptedCodec::HEADER_WIRE_SIZE + pn, n);

        let mut pbuf = [0u8; 1024];
        pbuf[..pn].copy_from_slice(
            &wire[EncryptedCodec::HEADER_WIRE_SIZE..EncryptedCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = r_codec
            .decode_frame(header, &mut pbuf[..pn])
            .expect("decode_frame");
        let received: SetupConnection<'_> = frame_to_message(frame).expect("frame_to_message");

        assert_eq!(received, outbound);
    }

    use ii_sv2_framing::{FramingError, IntoFrame, MessageDescriptor};
    use ii_sv2_messages::MINING_EXTENSION_TYPE;
    use ii_sv2_messages::{NewMiningJob, OpenStandardMiningChannel, SetupConnectionSuccess};
    use ii_sv2_types::Uint256;

    /// Synthetic frame whose `body_len()` returns a value larger than
    /// `MAX_PLAINTEXT_LENGTH`. `write_body` is `unreachable!()` because
    /// the encode guard must fire first.
    struct OversizedFrame;
    impl IntoFrame for OversizedFrame {
        fn descriptor(&self) -> MessageDescriptor {
            MessageDescriptor::new(MINING_EXTENSION_TYPE, 0xff, false)
        }
        fn body_len(&self) -> usize {
            MAX_PLAINTEXT_LENGTH + 1
        }
        fn write_body(&self, _out: &mut [u8]) -> Result<(), FramingError> {
            unreachable!("encode rejects before write_body for oversized frames")
        }
    }

    const TARGET_PATTERN: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    /// Helper: encode `frame` from `tx`, decrypt the header+payload into
    /// `wire`/`pbuf` via `rx`, and return `(payload_len, header)`.
    /// The decrypted plaintext payload lives in `pbuf[..payload_len]`.
    /// Callers call `rx.decode_frame` + `frame_to_message` themselves so
    /// that borrow-checker lifetimes are controlled by the call site.
    async fn send_recv_raw<F: ii_sv2_framing::IntoFrame>(
        tx: &mut EncryptedCodec,
        rx: &mut EncryptedCodec,
        frame: &F,
        wire: &mut [u8],
        pbuf: &mut [u8],
    ) -> (usize, ii_sv2_framing::Header) {
        let n = tx.encode(frame, wire).expect("encode");

        let mut hbuf = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..EncryptedCodec::HEADER_WIRE_SIZE]);
        let header = rx.decode_header(&mut hbuf).expect("decode_header");

        let pn = EncryptedCodec::payload_wire_size(header.msg_length as usize);
        assert_eq!(EncryptedCodec::HEADER_WIRE_SIZE + pn, n);

        pbuf[..pn].copy_from_slice(
            &wire[EncryptedCodec::HEADER_WIRE_SIZE..EncryptedCodec::HEADER_WIRE_SIZE + pn],
        );
        (pn, header)
    }

    #[tokio::test]
    async fn sequential_frames_advance_state() {
        let (mut i_codec, mut r_codec) = paired_codecs().await;

        let mut wire = [0u8; 1024];
        let mut pbuf = [0u8; 1024];

        // Frame 1: SetupConnectionSuccess (no lifetime, fixed-size).
        let success = SetupConnectionSuccess {
            used_version: 2,
            flags: 0,
        };
        {
            let (pn, header) =
                send_recv_raw(&mut i_codec, &mut r_codec, &success, &mut wire, &mut pbuf).await;
            let fv = r_codec
                .decode_frame(header, &mut pbuf[..pn])
                .expect("decode_frame");
            let received: SetupConnectionSuccess = frame_to_message(fv).expect("frame_to_message");
            assert_eq!(received, success);
        }

        // Frame 2: OpenStandardMiningChannel.
        let open = OpenStandardMiningChannel {
            request_id: 1,
            user: Str0_255::try_from("user.worker1").unwrap(),
            nominal_hashrate: 1.0e12,
            max_target: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        {
            let (pn, header) =
                send_recv_raw(&mut i_codec, &mut r_codec, &open, &mut wire, &mut pbuf).await;
            let fv = r_codec
                .decode_frame(header, &mut pbuf[..pn])
                .expect("decode_frame");
            let received: OpenStandardMiningChannel<'_> =
                frame_to_message(fv).expect("frame_to_message");
            assert_eq!(received, open);
        }

        // Frame 3: NewMiningJob (channel-message bit set).
        let job = NewMiningJob {
            channel_id: 100,
            job_id: 7,
            min_ntime: Some(1_700_000_000),
            version: 0x2000_0000,
            merkle_root: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        {
            let (pn, header) =
                send_recv_raw(&mut i_codec, &mut r_codec, &job, &mut wire, &mut pbuf).await;
            let fv = r_codec
                .decode_frame(header, &mut pbuf[..pn])
                .expect("decode_frame");
            let received: NewMiningJob<'_> = frame_to_message(fv).expect("frame_to_message");
            assert_eq!(received, job);
        }
    }

    #[tokio::test]
    async fn encode_rejects_oversized_payload() {
        let (mut i_codec, _r_codec) = paired_codecs().await;
        // Buffer is generously sized so we can be sure the rejection comes
        // from the chunking guard, not the buffer-size check.
        let mut wire = [0u8; 256];
        let err = i_codec.encode(&OversizedFrame, &mut wire).unwrap_err();
        match err {
            SessionError::ChunkingNotImplemented { length } => {
                assert_eq!(length, MAX_PLAINTEXT_LENGTH + 1);
            }
            other => panic!("expected ChunkingNotImplemented, got {:?}", other),
        }
    }

    use ii_sv2_framing::MessageGroup;
    use ii_sv2_messages::MiningMessages;

    #[tokio::test]
    async fn dispatch_via_mining_messages() {
        let (mut i_codec, mut r_codec) = paired_codecs().await;

        let outbound = NewMiningJob {
            channel_id: 100,
            job_id: 7,
            min_ntime: Some(1),
            version: 0,
            merkle_root: Uint256::from_le_bytes(&TARGET_PATTERN),
        };

        let mut wire = [0u8; 256];
        let n = i_codec.encode(&outbound, &mut wire).expect("encode");

        let mut hbuf = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..EncryptedCodec::HEADER_WIRE_SIZE]);
        let header = r_codec.decode_header(&mut hbuf).expect("decode_header");
        assert!(header.is_channel_message);

        let pn = EncryptedCodec::payload_wire_size(header.msg_length as usize);
        assert_eq!(EncryptedCodec::HEADER_WIRE_SIZE + pn, n);

        let mut pbuf = [0u8; 256];
        pbuf[..pn].copy_from_slice(
            &wire[EncryptedCodec::HEADER_WIRE_SIZE..EncryptedCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = r_codec
            .decode_frame(header, &mut pbuf[..pn])
            .expect("decode_frame");

        match MiningMessages::dispatch(frame).expect("dispatch") {
            MiningMessages::NewMiningJob(m) => assert_eq!(m, outbound),
            other => panic!("expected NewMiningJob, got {:?}", other),
        }
    }
}
