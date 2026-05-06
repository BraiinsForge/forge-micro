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

    fn encode<F: IntoFrame>(
        &mut self,
        frame: &F,
        out: &mut [u8],
    ) -> Result<usize, Self::Error> {
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
