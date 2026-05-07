//! `StratumV2Client` — Stratum V2 adapter wrapping `ii-sv2`.
//!
//! This commit lands `connect`: noise NX handshake via
//! `ii_sv2::security::initiate`, then synchronous `SetupConnection`
//! and `OpenStandardMiningChannel` over the `EncryptedCodec` that
//! results. `drive` and `accept` arrive in the next commits;
//! `submit` lands once `SubmitSharesStandard` is in `ii-sv2`.

use embedded_io_async::{Read, Write};
use ii_sv2::framing::{FrameCodec, Header};
use ii_sv2::messages::{DeviceInfo, OpenStandardMiningChannel, SetupConnection};
use ii_sv2::security::{
    initiate, secp256k1, CryptoCtx, EncryptedCodec, EstablishedSession, PublicKey, Rng,
    ValidateNothing, ValidateSignature,
};
use ii_sv2::types::{Str0_255, Uint256};
use mining_protocol::{Error, MiningClient, MiningClientVisitor, Share};

use crate::auth::AuthChoice;
use crate::config::Sv2Config;

/// Encrypted SV2 header on the wire: 6-byte header + 16-byte MAC.
/// `EncryptedCodec::HEADER_WIRE_SIZE` matches; reserved here as a
/// const so the receive-side header buffer can be sized.
const ENCRYPTED_HEADER_BYTES: usize = 22;

/// AEAD MAC bytes appended to each encrypted chunk.
const MAC_BYTES: usize = 16;

/// Wire buffer sized for the largest standard-channel mining message
/// plus the SV2 noise framing overhead. Tuned alongside the codec
/// impl in the next commits — 1 KiB is generous for the read path.
pub const WIRE_BUFFER_BYTES: usize = 1024;

/// Outbound buffer for one encoded message. `SetupConnection`
/// (~50 B wire), `OpenStandardMiningChannel` (~60 B wire), and the
/// future `SubmitSharesStandard` (~24 B wire) all fit several
/// times over.
pub const ENCODE_BUFFER_BYTES: usize = 512;

/// Most-permissive 32-byte target — `OpenStandardMiningChannel` asks
/// the pool to clamp its `SetTarget` value at this. The pool returns
/// the actual operating target in the channel-open success.
const PERMISSIVE_MAX_TARGET: [u8; 32] = [0xFF; 32];

/// Hardcoded `DeviceInfo` strings. The fields are advisory; pools
/// log them but don't gate on the values.
const DEVICE_VENDOR: &str = "braiins";
const DEVICE_HW_REV: &str = "rusty-boards";
const DEVICE_FW_VER: &str = env!("CARGO_PKG_VERSION");
const DEVICE_ID: &str = "";

/// Initial `request_id` for `OpenStandardMiningChannel`. The pool
/// echoes it in `OpenStandardMiningChannelSuccess.request_id`; the
/// adapter only opens one channel per session, so a fixed `1`
/// suffices.
const OPEN_CHANNEL_REQUEST_ID: u32 = 1;

/// Adapter state — `PreHandshake` until `connect` finishes the
/// noise dance, `Connected { codec }` afterwards.
enum V2State {
    PreHandshake,
    Connected {
        codec: EncryptedCodec,
        /// `channel_id` from `OpenStandardMiningChannelSuccess`,
        /// populated by `accept` once the response arrives. Until
        /// then, `submit` errors with `UnknownJobId`. Read in the
        /// `accept` dispatch (next commit).
        #[allow(dead_code)]
        channel_id: Option<u32>,
    },
}

/// Receive-side state machine. Each state describes precisely how
/// many bytes have been read off the wire and how far the AEAD
/// chain has advanced — invariant the cancellation safety relies
/// on.
///
/// `Idle` — no bytes read; AEAD at frame boundary. `ReadingHeader` —
/// `received` bytes of the encrypted 22-byte header are in the
/// header buffer; AEAD has NOT advanced. `ReadingPayload` — header
/// has been decrypted (AEAD advanced by one chunk); `received`
/// bytes of `header.msg_length + MAC_BYTES` are in the wire
/// buffer. `PayloadReady` — payload decrypted (AEAD advanced by a
/// second chunk); plaintext sits in the wire buffer waiting for
/// `accept` to dispatch.
#[derive(Clone, Copy)]
enum FrameState {
    Idle,
    ReadingHeader {
        received: usize,
    },
    ReadingPayload {
        header: Header,
        received: usize,
    },
    /// `header` is read by the next commit's `accept` dispatch when
    /// it builds a `FrameView` from the decrypted payload.
    PayloadReady {
        #[allow(dead_code)]
        header: Header,
    },
}

/// Stratum V2 protocol adapter. Generic over the platform RNG and
/// the secp256k1 context flavor (`secp256k1::All` on alloc-enabled
/// hosts; `secp256k1::AllPreallocated<'static>` on `no_std`
/// embedded targets).
pub struct StratumV2Client<R: Rng, C: secp256k1::Signing + secp256k1::Verification + 'static> {
    config: &'static Sv2Config,
    ctx: &'static CryptoCtx<C>,
    rng: Option<R>,
    state: V2State,
    /// Receive buffer for the encrypted payload — sized for the
    /// largest standard-channel frame plus the trailing MAC.
    wire_buffer: [u8; WIRE_BUFFER_BYTES],
    /// Receive buffer for the encrypted header (separate so the
    /// header decrypt can complete while leaving the wire buffer
    /// free to accumulate the payload).
    header_buffer: [u8; ENCRYPTED_HEADER_BYTES],
    /// Receive-side state machine; see `FrameState` for the
    /// invariant.
    frame_state: FrameState,
    encode_buffer: [u8; ENCODE_BUFFER_BYTES],
}

impl<R: Rng, C: secp256k1::Signing + secp256k1::Verification + 'static> StratumV2Client<R, C> {
    /// Construct an adapter ready for `connect`.
    pub const fn new(config: &'static Sv2Config, ctx: &'static CryptoCtx<C>, rng: R) -> Self {
        Self {
            config,
            ctx,
            rng: Some(rng),
            state: V2State::PreHandshake,
            wire_buffer: [0; WIRE_BUFFER_BYTES],
            header_buffer: [0; ENCRYPTED_HEADER_BYTES],
            frame_state: FrameState::Idle,
            encode_buffer: [0; ENCODE_BUFFER_BYTES],
        }
    }

    /// Build the authenticator the noise handshake will run against.
    /// `pool_pubkey = None` accepts any responder certificate (test
    /// pools); `Some(key)` validates the certificate's signature
    /// against `key`.
    fn build_authenticator(&self) -> AuthChoice<'static, C> {
        match self.config.pool_pubkey {
            None => AuthChoice::Nothing(ValidateNothing),
            Some(key) => AuthChoice::Signature(ValidateSignature {
                authority: PublicKey::new(key),
                ctx: self.ctx.as_secp256k1(),
            }),
        }
    }

    /// Encode `message` into `self.encode_buffer` via the
    /// post-handshake `codec`, then write the bytes to `transport`
    /// in a resumable loop. Used by `connect` for the two
    /// post-handshake handshake messages; future submit/share
    /// flushes go through `drive`'s outbound slot.
    async fn encode_and_send<F, T>(
        encode_buffer: &mut [u8; ENCODE_BUFFER_BYTES],
        codec: &mut EncryptedCodec,
        message: &F,
        transport: &mut T,
    ) -> Result<(), Error>
    where
        F: ii_sv2::framing::IntoFrame,
        T: Write,
    {
        let length = codec
            .encode(message, encode_buffer.as_mut_slice())
            .map_err(|_| Error::BufferTooSmall)?;
        let mut offset = 0;
        while offset < length {
            let written = transport
                .write(&encode_buffer[offset..length])
                .await
                .map_err(|_| Error::Transport)?;
            if written == 0 {
                return Err(Error::Transport);
            }
            offset += written;
        }
        Ok(())
    }
}

impl<R: Rng, C: secp256k1::Signing + secp256k1::Verification + 'static> MiningClient
    for StratumV2Client<R, C>
{
    /// Drive the noise handshake to completion, then send the two
    /// post-handshake messages every standard-channel session needs:
    /// `SetupConnection` followed by `OpenStandardMiningChannel`.
    /// Their responses arrive over the regular `drive`/`accept`
    /// loop.
    async fn connect<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        let rng = self.rng.take().ok_or(Error::Adapter)?;
        let authenticator = self.build_authenticator();

        let session: EstablishedSession = initiate(transport, rng, self.ctx, authenticator)
            .await
            .map_err(|err| map_init_error(&err))?;
        let (mut codec, _session_id) = EncryptedCodec::from_handshake(session);

        // SetupConnection — protocol = 0 (mining), version 2/2,
        // flags = 0 (no extensions requested initially), endpoint
        // empty (pool routing is handled at the TCP layer here).
        let setup = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: Str0_255::try_from("").map_err(|_| Error::Adapter)?,
            endpoint_port: 0,
            device: DeviceInfo {
                vendor: Str0_255::try_from(DEVICE_VENDOR).map_err(|_| Error::Adapter)?,
                hw_rev: Str0_255::try_from(DEVICE_HW_REV).map_err(|_| Error::Adapter)?,
                fw_ver: Str0_255::try_from(DEVICE_FW_VER).map_err(|_| Error::Adapter)?,
                dev_id: Str0_255::try_from(DEVICE_ID).map_err(|_| Error::Adapter)?,
            },
        };
        Self::encode_and_send(&mut self.encode_buffer, &mut codec, &setup, transport).await?;

        // OpenStandardMiningChannel — request_id is fixed for the
        // single channel this session opens; the pool echoes it
        // back in the success response so we can correlate.
        let open = OpenStandardMiningChannel {
            request_id: OPEN_CHANNEL_REQUEST_ID,
            user: Str0_255::try_from(self.config.user).map_err(|_| Error::Adapter)?,
            nominal_hashrate: 0.0,
            max_target: Uint256::from_le_bytes(&PERMISSIVE_MAX_TARGET),
        };
        Self::encode_and_send(&mut self.encode_buffer, &mut codec, &open, transport).await?;

        self.state = V2State::Connected {
            codec,
            channel_id: None,
        };
        Ok(())
    }

    fn submit(&mut self, _share: &Share<'_>) -> Result<(), Error> {
        // Lands once SubmitSharesStandard is in ii-sv2 (micro-sv2
        // user story MINE004).
        Err(Error::NotImplemented)
    }

    /// Read one encrypted SV2 frame off the transport and decrypt
    /// it in place. State machine drives in stages; each stage's
    /// only side-effect on AEAD state happens after the matching
    /// reads are complete, so a `drive` future cancelled mid-read
    /// can be resumed from the same state on the next call.
    async fn drive<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        let V2State::Connected { codec, .. } = &mut self.state else {
            // connect() hasn't completed yet; nothing to drive.
            return Err(Error::Adapter);
        };

        loop {
            match self.frame_state {
                FrameState::Idle => {
                    self.frame_state = FrameState::ReadingHeader { received: 0 };
                }
                FrameState::ReadingHeader { mut received } => {
                    while received < ENCRYPTED_HEADER_BYTES {
                        let read_count = transport
                            .read(&mut self.header_buffer[received..ENCRYPTED_HEADER_BYTES])
                            .await
                            .map_err(|_| Error::Transport)?;
                        if read_count == 0 {
                            return Err(Error::Transport);
                        }
                        received += read_count;
                        // Persist progress so a cancellation here
                        // doesn't lose the bytes already buffered.
                        self.frame_state = FrameState::ReadingHeader { received };
                    }
                    // Whole encrypted header is in `header_buffer`.
                    // Decrypt — first AEAD advance for this frame.
                    let header = codec
                        .decode_header(&mut self.header_buffer)
                        .map_err(|_| Error::Decode)?;
                    self.frame_state = FrameState::ReadingPayload {
                        header,
                        received: 0,
                    };
                }
                FrameState::ReadingPayload {
                    header,
                    mut received,
                } => {
                    let payload_total = header.msg_length as usize + MAC_BYTES;
                    if payload_total > self.wire_buffer.len() {
                        return Err(Error::Decode);
                    }
                    while received < payload_total {
                        let read_count = transport
                            .read(&mut self.wire_buffer[received..payload_total])
                            .await
                            .map_err(|_| Error::Transport)?;
                        if read_count == 0 {
                            return Err(Error::Transport);
                        }
                        received += read_count;
                        self.frame_state = FrameState::ReadingPayload { header, received };
                    }
                    // Whole encrypted payload buffered. Decrypt
                    // in place — second AEAD advance.
                    codec
                        .decode_frame(header, &mut self.wire_buffer[..payload_total])
                        .map_err(|_| Error::Decode)?;
                    self.frame_state = FrameState::PayloadReady { header };
                    return Ok(());
                }
                FrameState::PayloadReady { .. } => {
                    // Caller hasn't drained the previous frame
                    // through `accept` yet — pairing is required
                    // for AEAD alignment with the peer.
                    return Err(Error::Adapter);
                }
            }
        }
    }

    /// Dispatch the most recently decrypted frame to `visitor`.
    /// `MiningMessages` dispatch + translation to `MiningJob` /
    /// `SessionUpdate` / `SubmitResult` lands in the next commit.
    /// For now `accept` only resets the receive state machine so
    /// the next `drive` can read another frame; visitor stays
    /// silent.
    fn accept<V: MiningClientVisitor>(&mut self, _visitor: &mut V) {
        if matches!(self.frame_state, FrameState::PayloadReady { .. }) {
            self.frame_state = FrameState::Idle;
        }
    }
}

/// Coarse mapping from the rich `InitializationError` to our trait's
/// `Error` set. The adapter logs the underlying detail via defmt
/// before this conversion happens; the trait surface stays narrow.
fn map_init_error<E>(err: &ii_sv2::security::InitializationError<E>) -> Error {
    match err {
        ii_sv2::security::InitializationError::Authentication(_) => Error::AuthFailed,
        ii_sv2::security::InitializationError::Io(_)
        | ii_sv2::security::InitializationError::UnexpectedEof => Error::Transport,
        ii_sv2::security::InitializationError::Handshake(_)
        | ii_sv2::security::InitializationError::InvalidKey(_) => Error::SessionRejected,
    }
}
