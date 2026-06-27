//! `StratumV2Client` — Stratum V2 adapter wrapping `ii-sv2`.
//!
//! `connect` performs the noise NX handshake via
//! `ii_sv2::security::initiate`, then sends `SetupConnection` and
//! `OpenStandardMiningChannel` over the `EncryptedCodec` it
//! produces. `drive` flushes any queued outbound frame and reads
//! one encrypted frame back, decrypting it in place. `accept`
//! dispatches the most recently buffered frame to the visitor.
//! `submit` encodes `SubmitSharesStandard` into the outbound slot
//! for the next `drive` to flush.

use embedded_io_async::{Read, Write};
use ii_sv2::framing::{FrameCodec, FrameView, Header, MessageGroup};
use ii_sv2::messages::{
    DeviceInfo, MiningMessages, NewMiningJob, OpenStandardMiningChannel,
    OpenStandardMiningChannelSuccess, SetNewPrevHash, SetTarget, SetupConnection,
    SubmitSharesError, SubmitSharesStandard, SubmitSharesSuccess,
};
use ii_sv2::security::{
    CryptoCtx, EncryptedCodec, EstablishedSession, PublicKey, Rng, ValidateNothing,
    ValidateSignature, initiate, secp256k1,
};
use ii_sv2::types::{Str0_255, Uint256};
use mining_protocol::{
    Error, LazyMerkle, MiningClient, MiningClientVisitor, MiningJob, SessionRejection,
    SessionUpdate, Share, SubmitResult,
};
use ubos_logging::{info, warn};

use crate::auth::AuthChoice;
use crate::config::Sv2Config;
use crate::state::{MAX_EXTRANONCE_PREFIX, PendingFutureJob, V2SessionState};

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
/// Hand-maintained firmware identity, intentionally decoupled from
/// `CARGO_PKG_VERSION`. The field is advisory, and deriving it from the
/// crate semver churned the on-wire bytes and the golden transcript
/// fixtures on every patch bump. Bump it deliberately when advertising a
/// new firmware identity to pools.
const DEVICE_FW_VER: &str = "0.1.0";
const DEVICE_ID: &str = "";

/// Initial `request_id` for `OpenStandardMiningChannel`. The pool
/// echoes it in `OpenStandardMiningChannelSuccess.request_id`; the
/// adapter only opens one channel per session, so a fixed `1`
/// suffices.
const OPEN_CHANNEL_REQUEST_ID: u32 = 1;

/// BIP320 version-rolling mask. The 16 bits between bit 13 and bit 28
/// that miners may flip without invalidating consensus rules. SV2
/// standard channels grant this mask when the upstream advertises
/// `REQUIRES_VERSION_ROLLING` in `SetupConnectionSuccess.flags`.
const BIP320_VERSION_MASK: u32 = 0x1FFF_E000;

/// Adapter state — `PreHandshake` until `connect` finishes the
/// noise dance, `Connected { codec, session }` afterwards.
//
// `Connected` is the dominant variant once the handshake
// completes; the enum lives in one static slot on
// `StratumV2Client`, so the size delta from `PreHandshake` is
// allocated unconditionally either way. No alloc is available on
// embedded targets, so `Box`ing is not an option.
#[allow(clippy::large_enum_variant)]
enum V2State {
    PreHandshake,
    Connected {
        codec: EncryptedCodec,
        session: V2SessionState,
    },
    /// Terminal: the pool sent a fatal error response (either
    /// `SetupConnectionError` or `OpenMiningChannelError`). All
    /// further `drive` / `submit` calls return
    /// `Error::SessionRejected(cause)` so the work loop tears the
    /// session down and reconnects. Cause is retained verbatim so
    /// the caller can log / decide based on which wire message
    /// ended the session.
    Rejected(SessionRejection),
}

/// Receive-side state machine. Each state describes precisely how
/// many bytes have been read off the wire and how far the AEAD
/// chain has advanced — invariant the cancellation safety relies
/// on.
///
/// `ReadingHeader` — `received` bytes of the encrypted 22-byte
/// header are in the header buffer; AEAD has NOT advanced. The
/// initial state (`received: 0`) is also "ready for the next
/// frame" after `accept`. `ReadingPayload` — header has been
/// decrypted (AEAD advanced by one chunk); `received` bytes of
/// `header.msg_length + MAC_BYTES` are in the wire buffer.
/// `PayloadReady` — payload decrypted (AEAD advanced by a second
/// chunk); plaintext sits in the wire buffer waiting for `accept`
/// to dispatch. `Broken` — terminal sink for any error path
/// (transport failure, decrypt failure, oversized payload); the
/// next `drive` returns `Error::Decode` immediately without
/// touching the wire, so a poisoned AEAD chain or dead transport
/// can't be retried by accident.
#[derive(Clone, Copy)]
enum FrameState {
    ReadingHeader {
        received: usize,
    },
    ReadingPayload {
        header: Header,
        received: usize,
    },
    /// `header` is consumed by the next `accept` call to build the
    /// `FrameView` over the decrypted payload.
    PayloadReady {
        header: Header,
    },
    Broken,
}

/// Stratum V2 protocol adapter. Generic over the platform RNG and
/// the secp256k1 context flavor (`secp256k1::All` on alloc-enabled
/// hosts; `secp256k1::AllPreallocated<'static>` on `no_std`
/// embedded targets). `'a` ties the held references to the
/// caller-owned config and crypto context — `&'static` works
/// (firmware statics) but is no longer required (tests can use
/// shorter-lived buffers).
pub struct StratumV2Client<'a, R: Rng, C: secp256k1::Signing + secp256k1::Verification> {
    config: &'a Sv2Config,
    ctx: &'a CryptoCtx<C>,
    rng: Option<R>,
    state: V2State,
    /// Receive buffer for the encrypted payload — sized for the
    /// largest standard-channel frame plus the trailing MAC.
    wire_buffer: [u8; WIRE_BUFFER_BYTES],
    /// Receive buffer for the encrypted header (separate so the
    /// header decrypt can complete while leaving the wire buffer
    /// free to accumulate the payload).
    header_buffer: [u8; EncryptedCodec::HEADER_WIRE_SIZE],
    /// Receive-side state machine; see `FrameState` for the
    /// invariant.
    frame_state: FrameState,
    encode_buffer: [u8; ENCODE_BUFFER_BYTES],
    /// Bytes pending flush in `encode_buffer`. `0` when nothing
    /// is queued. `submit` only encodes a fresh frame when this is
    /// `0`; otherwise it returns `BufferTooSmall` and waits for the
    /// next `drive` to flush the previous one.
    outbound_len: usize,
    /// Cursor into `encode_buffer[..outbound_len]` for resuming a
    /// partial flush across cancellation.
    outbound_offset: usize,
}

impl<'a, R: Rng, C: secp256k1::Signing + secp256k1::Verification> StratumV2Client<'a, R, C> {
    /// Construct an adapter ready for `connect`.
    pub const fn new(config: &'a Sv2Config, ctx: &'a CryptoCtx<C>, rng: R) -> Self {
        Self {
            config,
            ctx,
            rng: Some(rng),
            state: V2State::PreHandshake,
            wire_buffer: [0; WIRE_BUFFER_BYTES],
            header_buffer: [0; EncryptedCodec::HEADER_WIRE_SIZE],
            frame_state: FrameState::ReadingHeader { received: 0 },
            encode_buffer: [0; ENCODE_BUFFER_BYTES],
            outbound_len: 0,
            outbound_offset: 0,
        }
    }

    /// Build the authenticator the noise handshake will run against.
    /// `pool_pubkey = None` accepts any responder certificate (test
    /// pools); `Some(key)` validates the certificate's signature
    /// against `key`. The returned `AuthChoice` borrows from
    /// `self.ctx`; the noise handshake consumes it before the
    /// adapter's first `.await` returns control to the caller, so
    /// the borrow never escapes.
    fn build_authenticator(&self) -> AuthChoice<'a, C> {
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

impl<R: Rng, C: secp256k1::Signing + secp256k1::Verification> MiningClient
    for StratumV2Client<'_, R, C>
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
        info!(
            "stratum-v2: noise handshake start, authenticator={}",
            if self.config.pool_pubkey.is_some() {
                "validate-signature"
            } else {
                "validate-nothing"
            },
        );

        let session: EstablishedSession = initiate(transport, rng, self.ctx, authenticator)
            .await
            .map_err(|err| {
            log_init_error(&err);
            map_init_error(&err)
        })?;
        let (mut codec, _session_id) = EncryptedCodec::from_handshake(session);
        info!("stratum-v2: noise handshake complete");

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
        info!("stratum-v2: setup-connection sent (protocol=mining, version=2)");

        // OpenStandardMiningChannel — request_id is fixed for the
        // single channel this session opens; the pool echoes it
        // back in the success response so we can correlate.
        let open = OpenStandardMiningChannel {
            request_id: OPEN_CHANNEL_REQUEST_ID,
            user: Str0_255::try_from(self.config.user.as_str()).map_err(|_| Error::Adapter)?,
            nominal_hashrate: 0.0,
            max_target: Uint256::from_le_bytes(&PERMISSIVE_MAX_TARGET),
        };
        Self::encode_and_send(&mut self.encode_buffer, &mut codec, &open, transport).await?;
        info!(
            "stratum-v2: open-standard-mining-channel sent, request_id={}",
            OPEN_CHANNEL_REQUEST_ID,
        );

        self.state = V2State::Connected {
            codec,
            session: V2SessionState::new(),
        };
        Ok(())
    }

    /// Encode `SubmitSharesStandard` for `share` and queue it into
    /// `encode_buffer`. The next `drive` call flushes the bytes;
    /// the pool's `SubmitSharesSuccess` / `SubmitSharesError`
    /// response surfaces through `accept` → `visit_submit_result`.
    fn submit(&mut self, share: &Share<'_>) -> Result<(), Error> {
        if let V2State::Rejected(cause) = self.state {
            return Err(Error::SessionRejected(cause));
        }
        if self.outbound_len > 0 {
            // Previous submit hasn't flushed yet; the encrypted
            // codec is single-chunk, so we don't pipeline.
            warn!(
                "stratum-v2: submit dropped, outbound buffer in use ({}B pending)",
                self.outbound_len - self.outbound_offset,
            );
            return Err(Error::BufferTooSmall);
        }
        let V2State::Connected { codec, session } = &mut self.state else {
            warn!("stratum-v2: submit dropped, session not connected");
            return Err(Error::Adapter);
        };
        let Some(channel_id) = session.channel_id else {
            warn!("stratum-v2: submit dropped, channel not yet opened");
            return Err(Error::Adapter);
        };
        let Some(job) = session.lookup_job(share.id) else {
            warn!(
                "stratum-v2: submit dropped, JobId({}) not in ring",
                share.id.0,
            );
            return Err(Error::UnknownJobId);
        };
        let sequence_number = session.allocate_sequence_number();

        // `SubmitSharesStandard.version` is the full nVersion the chip
        // hashed, not just the rolled bits. The chip rolls the BIP320
        // field; restore the pool's base version in the remaining bits
        // so the pool recomputes the same header hash.
        let full_version =
            (job.version & !BIP320_VERSION_MASK) | (share.version_bits & BIP320_VERSION_MASK);

        let message = SubmitSharesStandard {
            channel_id,
            sequence_number,
            job_id: job.wire,
            nonce: share.nonce,
            ntime: share.ntime,
            version: full_version,
        };
        let length = codec
            .encode(&message, self.encode_buffer.as_mut_slice())
            .map_err(|_| Error::BufferTooSmall)?;
        self.outbound_len = length;
        self.outbound_offset = 0;
        info!(
            "stratum-v2: submit queued channel={} seq={} job={} \
             nonce={:#010x} ntime={:#010x} version={:#010x}",
            channel_id, sequence_number, job.wire, share.nonce, share.ntime, full_version,
        );
        Ok(())
    }

    /// Flush any queued outbound bytes, then read one encrypted
    /// SV2 frame off the transport and decrypt it in place. The
    /// receive-side state machine drives in stages; each stage's
    /// only side-effect on AEAD state happens after the matching
    /// reads are complete, so a `drive` future cancelled mid-read
    /// can be resumed from the same state on the next call.
    /// Outbound flushing is similarly resumable via
    /// `outbound_offset`.
    #[allow(clippy::too_many_lines)]
    async fn drive<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        if matches!(self.frame_state, FrameState::Broken) {
            // Earlier `drive` left the codec / transport in an
            // unrecoverable state. Don't touch the wire — let the
            // caller tear the session down on the second Err.
            return Err(Error::Decode);
        }
        if let V2State::Rejected(cause) = self.state {
            return Err(Error::SessionRejected(cause));
        }
        // Flush queued outbound bytes (e.g. a `SubmitSharesStandard`
        // produced by a recent `submit`) before reading. The bytes
        // are already AEAD-sealed, so a partial write is safe to
        // resume across cancellation; any transport error poisons
        // the state machine to match the read-side semantics.
        while self.outbound_offset < self.outbound_len {
            let Ok(written) = transport
                .write(&self.encode_buffer[self.outbound_offset..self.outbound_len])
                .await
            else {
                self.frame_state = FrameState::Broken;
                return Err(Error::Transport);
            };
            if written == 0 {
                self.frame_state = FrameState::Broken;
                return Err(Error::Transport);
            }
            self.outbound_offset += written;
        }
        if self.outbound_len > 0 {
            self.outbound_len = 0;
            self.outbound_offset = 0;
        }

        let V2State::Connected { codec, .. } = &mut self.state else {
            // connect() hasn't completed yet; nothing to drive.
            return Err(Error::Adapter);
        };

        loop {
            match self.frame_state {
                FrameState::ReadingHeader { ref mut received } => {
                    while *received < EncryptedCodec::HEADER_WIRE_SIZE {
                        let Ok(read_count) = transport
                            .read(
                                &mut self.header_buffer
                                    [*received..EncryptedCodec::HEADER_WIRE_SIZE],
                            )
                            .await
                        else {
                            self.frame_state = FrameState::Broken;
                            return Err(Error::Transport);
                        };
                        if read_count == 0 {
                            self.frame_state = FrameState::Broken;
                            return Err(Error::Transport);
                        }
                        *received += read_count;
                    }
                    // Whole encrypted header is in `header_buffer`.
                    // Park state in `Broken` before the fallible
                    // decrypt: on `?` the machine stays Broken; on
                    // success we transition to `ReadingPayload`.
                    self.frame_state = FrameState::Broken;
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
                    ref mut received,
                } => {
                    let payload_total = header.msg_length as usize + MAC_BYTES;
                    if payload_total > self.wire_buffer.len() {
                        self.frame_state = FrameState::Broken;
                        return Err(Error::Decode);
                    }
                    while *received < payload_total {
                        let Ok(read_count) = transport
                            .read(&mut self.wire_buffer[*received..payload_total])
                            .await
                        else {
                            self.frame_state = FrameState::Broken;
                            return Err(Error::Transport);
                        };
                        if read_count == 0 {
                            self.frame_state = FrameState::Broken;
                            return Err(Error::Transport);
                        }
                        *received += read_count;
                    }
                    // Same Broken-park pattern as for the header.
                    self.frame_state = FrameState::Broken;
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
                FrameState::Broken => {
                    // Guarded at the top; unreachable in steady
                    // state but keeps the match exhaustive.
                    return Err(Error::Decode);
                }
            }
        }
    }

    /// Dispatch the most recently decrypted frame to `visitor`.
    ///
    /// Builds a `FrameView` over the plaintext that `drive` left in
    /// `wire_buffer`, translates the `MiningMessages` variant into
    /// the visitor's canonical `MiningJob` / `SessionUpdate` shape,
    /// and resets the receive state machine so the next `drive` can
    /// read another frame. Parse or dispatch failures emit a
    /// warning and otherwise drop the frame.
    #[allow(clippy::too_many_lines)]
    fn accept<V: MiningClientVisitor>(&mut self, visitor: &mut V) {
        let FrameState::PayloadReady { header } = self.frame_state else {
            return;
        };
        // Arm the state machine for the next frame: even on parse
        // failure the next drive must be free to read.
        self.frame_state = FrameState::ReadingHeader { received: 0 };

        let payload_len = header.msg_length as usize;
        if payload_len > self.wire_buffer.len() {
            warn!(
                "stratum-v2: frame dropped, payload_len {} > wire buffer {}",
                payload_len,
                self.wire_buffer.len(),
            );
            return;
        }
        let Ok(frame) = FrameView::try_new(header, &self.wire_buffer[..payload_len]) else {
            warn!(
                "stratum-v2: FrameView::try_new failed (msg_length={})",
                header.msg_length,
            );
            return;
        };
        let Ok(message) = MiningMessages::dispatch(frame) else {
            warn!(
                "stratum-v2: unknown message ext={:#x} type={:#x} channel={}",
                header.extension_type, header.msg_type, header.is_channel_message,
            );
            return;
        };

        let V2State::Connected { session, .. } = &mut self.state else {
            return;
        };

        match message {
            MiningMessages::SetupConnectionSuccess(success) => {
                info!(
                    "stratum-v2: setup-connection-success used_version={} flags={:#010x}",
                    success.used_version, success.flags,
                );
                let mask = if success.flags & SetupConnection::REQUIRES_VERSION_ROLLING != 0 {
                    BIP320_VERSION_MASK
                } else {
                    0
                };
                visitor.visit_session_update(SessionUpdate::SetVersionMask(mask));
            }
            MiningMessages::SetupConnectionError(error) => {
                warn!(
                    "stratum-v2: setup-connection-error flags={:#010x} code={}",
                    error.flags,
                    core::str::from_utf8(error.code.as_bytes()).unwrap_or("<invalid utf-8>"),
                );
                self.state = V2State::Rejected(SessionRejection::SetupConnectionError);
            }
            MiningMessages::OpenMiningChannelError(error) => {
                warn!(
                    "stratum-v2: open-mining-channel-error req={} code={}",
                    error.request_id,
                    core::str::from_utf8(error.code.as_bytes()).unwrap_or("<invalid utf-8>"),
                );
                self.state = V2State::Rejected(SessionRejection::OpenMiningChannelError);
            }
            MiningMessages::OpenStandardMiningChannelSuccess(success) => {
                let prefix_len = success.extranonce_prefix.as_slice().len();
                if prefix_len > MAX_EXTRANONCE_PREFIX {
                    warn!(
                        "stratum-v2: open-channel-success extranonce_prefix={}B > MAX={}B; ending session",
                        prefix_len, MAX_EXTRANONCE_PREFIX,
                    );
                    self.state = V2State::Rejected(SessionRejection::ExtranonceTooLarge);
                } else {
                    handle_channel_open_success(session, &success, visitor);
                }
            }
            MiningMessages::SetTarget(set_target) => {
                handle_set_target(session, &set_target, visitor);
            }
            MiningMessages::SetNewPrevHash(snph) => {
                handle_set_new_prev_hash(session, &snph, visitor);
            }
            MiningMessages::NewMiningJob(job) => {
                handle_new_mining_job(session, &job, visitor);
            }
            MiningMessages::SubmitSharesSuccess(success) => {
                handle_submit_success(&success, visitor);
            }
            MiningMessages::SubmitSharesError(error) => {
                handle_submit_error(&error, visitor);
            }
            // Client-side echoes; the pool never sends these to
            // a miner. Silently drop.
            MiningMessages::SetupConnection(_)
            | MiningMessages::OpenStandardMiningChannel(_)
            | MiningMessages::SubmitSharesStandard(_) => {}
        }
    }
}

/// `OpenStandardMiningChannelSuccess` carries the operating
/// `channel_id`, the initial `target`, and the `extranonce_prefix`
/// the standard channel will use. All three are persisted on
/// `session`; the visitor then sees `SetTarget` and `SetExtranonce`
/// updates so the integration layer can stamp them into the next
/// `MiningJob`.
fn handle_channel_open_success<V: MiningClientVisitor>(
    session: &mut V2SessionState,
    success: &OpenStandardMiningChannelSuccess<'_>,
    visitor: &mut V,
) {
    session.channel_id = Some(success.channel_id);
    session.target.copy_from_slice(success.target.to_le_bytes());
    let prefix = success.extranonce_prefix.as_slice();
    // The accept-side validation rejects the session if
    // `prefix.len() > MAX_EXTRANONCE_PREFIX`, so this fits exactly.
    let prefix_len = prefix.len();
    debug_assert!(prefix_len <= MAX_EXTRANONCE_PREFIX);
    session.extranonce_prefix_len = prefix_len;
    session.extranonce_prefix[..prefix_len].copy_from_slice(prefix);
    {
        let target_hex = mining_protocol::hex::encode_block_hash_lower(&session.target);
        let diff = mining_protocol::difficulty::approx_string(&session.target);
        let mut prefix_hex = [0u8; MAX_EXTRANONCE_PREFIX * 2];
        let prefix_hex_len = prefix_len * 2;
        let prefix_hex_ok = mining_protocol::hex::encode_lower_to_slice(
            &session.extranonce_prefix[..prefix_len],
            &mut prefix_hex[..prefix_hex_len],
        );
        debug_assert!(prefix_hex_ok);
        info!(
            "stratum-v2: channel-open-success channel={} prefix={} \
             target={}... diff={}",
            success.channel_id,
            core::str::from_utf8(&prefix_hex[..prefix_hex_len]).expect("hex encoder emits ASCII"),
            core::str::from_utf8(&target_hex[..32]).expect("hex encoder emits ASCII"),
            diff.as_str(),
        );
    }
    visitor.visit_session_update(SessionUpdate::SetTarget(&session.target));
    visitor.visit_session_update(SessionUpdate::SetExtranonce {
        prefix: &session.extranonce_prefix[..session.extranonce_prefix_len],
        en2_size: 0,
    });
}

/// Mid-session `SetTarget` overwrites the channel target.
fn handle_set_target<V: MiningClientVisitor>(
    session: &mut V2SessionState,
    set_target: &SetTarget<'_>,
    visitor: &mut V,
) {
    session
        .target
        .copy_from_slice(set_target.max_target.to_le_bytes());
    {
        let target_hex = mining_protocol::hex::encode_block_hash_lower(&session.target);
        let diff = mining_protocol::difficulty::approx_string(&session.target);
        info!(
            "stratum-v2: set-target channel={} target={}... diff={}",
            set_target.channel_id,
            core::str::from_utf8(&target_hex[..32]).expect("hex encoder emits ASCII"),
            diff.as_str(),
        );
    }
    visitor.visit_session_update(SessionUpdate::SetTarget(&session.target));
}

/// `SetNewPrevHash` advances the chain tip. It records the new
/// `prev_hash`, `nbits`, and `min_ntime` base, then activates any
/// pending future job whose wire `job_id` matches.
fn handle_set_new_prev_hash<V: MiningClientVisitor>(
    session: &mut V2SessionState,
    snph: &SetNewPrevHash<'_>,
    visitor: &mut V,
) {
    session
        .prev_hash
        .copy_from_slice(snph.prev_hash.to_le_bytes());
    session.have_prev_hash = true;
    session.nbits = snph.nbits;
    session.ntime_base = snph.min_ntime;
    {
        let prev_hex = mining_protocol::hex::encode_block_hash_lower(&session.prev_hash);
        info!(
            "stratum-v2: set-new-prev-hash channel={} job={} \
             nbits={:#010x} ntime={:#010x} prev_hash={}",
            snph.channel_id,
            snph.job_id,
            snph.nbits,
            snph.min_ntime,
            core::str::from_utf8(&prev_hex).expect("hex encoder emits ASCII"),
        );
    }

    let Some(pending) = session.pending_future_job.take() else {
        return;
    };
    if pending.wire_job_id != snph.job_id {
        // Future job was for a different job_id — drop it; the
        // pool will resend if it still applies.
        warn!(
            "stratum-v2: pending future job dropped, job_id mismatch \
             (pending={}, snph={})",
            pending.wire_job_id, snph.job_id,
        );
        return;
    }
    emit_mining_job(
        session,
        pending.wire_job_id,
        pending.version,
        &pending.merkle_root,
        snph.min_ntime,
        true,
        visitor,
    );
}

/// `NewMiningJob` either activates immediately (when `min_ntime` is
/// `Some`, meaning the job applies to the current `prev_hash`) or
/// is queued as a pending future job (when `min_ntime` is `None`,
/// meaning it activates with a later `SetNewPrevHash`).
fn handle_new_mining_job<V: MiningClientVisitor>(
    session: &mut V2SessionState,
    job: &NewMiningJob<'_>,
    visitor: &mut V,
) {
    if let Some(ntime) = job.min_ntime {
        if !session.have_prev_hash {
            // Pool sent an active job before any prev hash — drop;
            // the next `SetNewPrevHash` will reset state.
            warn!(
                "stratum-v2: new-mining-job dropped, no prev_hash yet (job={})",
                job.job_id,
            );
            return;
        }
        info!(
            "stratum-v2: new-mining-job channel={} job={} \
             version={:#010x} ntime={:#010x}",
            job.channel_id, job.job_id, job.version, ntime,
        );
        emit_mining_job(
            session,
            job.job_id,
            job.version,
            job.merkle_root.to_le_bytes(),
            ntime,
            false,
            visitor,
        );
        return;
    }

    let mut merkle_root = [0u8; 32];
    merkle_root.copy_from_slice(job.merkle_root.to_le_bytes());
    info!(
        "stratum-v2: new-mining-job pending channel={} job={} version={:#010x}",
        job.channel_id, job.job_id, job.version,
    );
    session.pending_future_job = Some(PendingFutureJob {
        wire_job_id: job.job_id,
        version: job.version,
        merkle_root,
    });
}

/// Allocate a `JobId`, build the canonical `MiningJob`, and hand it
/// to `visitor`. The `merkle_root` borrow can come from either the
/// wire buffer (immediate `NewMiningJob`) or the pending-future
/// slot — either is valid for the duration of the visitor call.
fn emit_mining_job<V: MiningClientVisitor>(
    session: &mut V2SessionState,
    wire_job_id: u32,
    version: u32,
    merkle_root: &[u8; 32],
    ntime: u32,
    clean_jobs: bool,
    visitor: &mut V,
) {
    let job_id = session.allocate_job_id(wire_job_id, version);
    let mining_job = MiningJob {
        id: job_id,
        prev_hash: &session.prev_hash,
        target: &session.target,
        version,
        nbits: session.nbits,
        ntime,
        clean_jobs,
        // V2 standard channels do not carry `extranonce_2` —
        // every share rolls only the nonce/ntime/version bits.
        extranonce_2_size: 0,
        merkle: LazyMerkle::Final(merkle_root),
    };
    visitor.visit_new_job(mining_job);
}

/// `SubmitSharesSuccess` covers one or more accepted shares; the
/// frame carries both how many submits the pool accepted in the
/// batch (`new_submits_accepted_count`) and the total share weight
/// it credited (`new_shares_sum`). The visitor sees both verbatim
/// via `SubmitResult::Accepted`, so V2 share accounting needs no
/// seq→`JobId` map.
fn handle_submit_success<V: MiningClientVisitor>(success: &SubmitSharesSuccess, visitor: &mut V) {
    info!(
        "stratum-v2: submit-shares-success channel={} last_seq={} \
         accepted_count={} shares_sum={}",
        success.channel_id,
        success.last_sequence_number,
        success.new_submits_accepted_count,
        success.new_shares_sum,
    );
    visitor.visit_submit_result(SubmitResult::Accepted {
        count: success.new_submits_accepted_count,
        work: success.new_shares_sum,
    });
}

/// `SubmitSharesError` carries a string code per SV2 spec
/// (`stale-share`, `invalid-job-id`, etc.). Surface it as
/// `error_message`; `error_code` stays `None` because the trait
/// uses a numeric V1 code we don't have a V2 counterpart for.
fn handle_submit_error<V: MiningClientVisitor>(error: &SubmitSharesError<'_>, visitor: &mut V) {
    let code_bytes = error.error_code.as_bytes();
    warn!(
        "stratum-v2: submit-shares-error channel={} seq={} code={}",
        error.channel_id,
        error.sequence_number,
        core::str::from_utf8(code_bytes).unwrap_or("<invalid utf-8>"),
    );
    let error_message = core::str::from_utf8(code_bytes).ok();
    visitor.visit_submit_result(SubmitResult::Rejected {
        error_code: None,
        error_message,
    });
}

/// Coarse mapping from the rich `InitializationError` to our trait's
/// `Error` set. The adapter logs the underlying detail via
/// `ubos_logging` before this conversion happens; the trait surface
/// stays narrow.
///
/// `Handshake` covers Noise framing / message-order failures that
/// happen before any V2 message has been exchanged, so it lands in
/// `Transport`. `InvalidKey` means our configured pool pubkey itself
/// failed to parse — a local config error rather than a pool-side
/// rejection — so it lands in `Adapter`.
fn map_init_error<E>(err: &ii_sv2::security::InitializationError<E>) -> Error {
    match err {
        ii_sv2::security::InitializationError::Authentication(_) => Error::AuthFailed,
        ii_sv2::security::InitializationError::Io(_)
        | ii_sv2::security::InitializationError::UnexpectedEof
        | ii_sv2::security::InitializationError::Handshake(_) => Error::Transport,
        ii_sv2::security::InitializationError::InvalidKey(_) => Error::Adapter,
    }
}

/// Log the failing handshake variant before `map_init_error` flattens
/// it. `InitializationError` does not implement `core::fmt::Debug` or
/// `defmt::Format` uniformly, so we discriminate the variants and log
/// a short label only.
fn log_init_error<E>(err: &ii_sv2::security::InitializationError<E>) {
    let label: &str = match err {
        ii_sv2::security::InitializationError::Authentication(_) => "authentication",
        ii_sv2::security::InitializationError::Io(_) => "io",
        ii_sv2::security::InitializationError::UnexpectedEof => "unexpected-eof",
        ii_sv2::security::InitializationError::Handshake(_) => "handshake",
        ii_sv2::security::InitializationError::InvalidKey(_) => "invalid-key",
    };
    warn!("stratum-v2: noise handshake failed, kind={}", label);
}
