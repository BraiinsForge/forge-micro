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

//! Integration tests for `StratumV2Client`.
//!
//! Tests drive the public `MiningClient` trait surface only. Internal
//! state (`V2State`, `FrameState`, `V2SessionState`) is invisible here;
//! assertions rely solely on return values and visitor callbacks.
//!
//! The fake-pool harness builds an in-memory `tokio::io::duplex` channel,
//! wraps the client end in `FromTokio`, and runs the SV2 noise responder
//! (`ii_sv2::security::accept`) on the pool end. The resulting
//! `EncryptedCodec` lets the pool side encode pool→client frames and
//! decode client→pool frames without any live network connection.

extern crate std;

use core::future::Future;
use core::task::{Context, Poll};
use std::vec::Vec;

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{ErrorType, Read, Write};
use ii_sv2::framing::{FrameCodec, FrameView, Header, MessageDescriptor, MessageGroup};
use ii_sv2::messages::{
    MiningMessages, NewMiningJob, OpenMiningChannelError, OpenStandardMiningChannelSuccess,
    SetNewPrevHash, SetupConnectionError, SetupConnectionSuccess, SubmitSharesError,
    SubmitSharesSuccess,
};
use ii_sv2::security::{
    CertificateWithPrivateKey, EncryptedCodec, EstablishedSession, Rng as Sv2Rng, accept, secp256k1,
};
use ii_sv2::types::{Bytes0_32, Str0_255, Uint256};
use mining_protocol::{Error, ExtensionFrame, JobId, MiningClient, SessionRejection, Share};
use rand::RngCore;
use stratum_v2::{MAX_EXTENSION_PAYLOAD_BYTES, StratumV2Client, Sv2Config};
use tokio::io::AsyncReadExt as _;

mod common;

use common::{
    FAKE_CHANNEL_ID, FAKE_EXTRANONCE_PREFIX, FAKE_JOB_ID, FAKE_MERKLE_ROOT, FAKE_NBITS, FAKE_NTIME,
    FAKE_PREV_HASH, FAKE_TARGET, POOL_WIRE_BUFFER_SIZE, RecordedSubmitResult, RecordedUpdate,
    RecordingVisitor, crypto_ctx, make_cert, pool_send,
};

// ── Rng impl ────────────────────────────────────────────────────────────────────

/// `Sv2Rng` backed by the OS CSPRNG via `rand`. Used for both the
/// client and the fake-pool responder during every test handshake.
struct OsRng;

impl Sv2Rng for OsRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand::rngs::OsRng.fill_bytes(dest);
    }
}

// ── Self-signed certificate ──────────────────────────────────────────────────────

/// Build a fresh self-signed `CertificateWithPrivateKey` for the fake pool.
/// Each call draws new OS-random keypairs so tests are independent.
fn make_server_cert() -> CertificateWithPrivateKey {
    make_cert(&mut OsRng, &mut OsRng)
}

// ── Fake-pool wire helpers ───────────────────────────────────────────────────────

/// Read one SV2 frame from `reader` and decode it with `codec`.
/// Returns a `MiningMessages<'static>` variant by leaking the payload bytes.
async fn pool_recv(
    codec: &mut EncryptedCodec,
    reader: &mut tokio::io::DuplexStream,
) -> MiningMessages<'static> {
    let mut header_bytes = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
    reader
        .read_exact(&mut header_bytes)
        .await
        .expect("BUG: pool_recv header read_exact failed");
    let header = codec
        .decode_header(&mut header_bytes)
        .expect("BUG: pool_recv decode_header failed");

    let payload_wire_size = EncryptedCodec::payload_wire_size(header.msg_length as usize);
    let mut payload = vec![0u8; payload_wire_size];
    reader
        .read_exact(&mut payload)
        .await
        .expect("BUG: pool_recv payload read_exact failed");
    codec
        .decode_frame(header, &mut payload)
        .expect("BUG: pool_recv decode_frame failed");

    let plaintext_len = header.msg_length as usize;
    let owned: Vec<u8> = payload[..plaintext_len].to_vec();
    // Leak the payload vec to produce a `'static` slice for the returned
    // `MiningMessages<'static>`. This is intentional in tests: each call
    // site leaks a small amount of memory that the test process reclaims
    // on exit.
    let slice_ref: &'static [u8] = Vec::leak(owned);
    let frame_view = ii_sv2::framing::FrameView::try_new(header, &slice_ref[..plaintext_len])
        .expect("BUG: pool_recv FrameView::try_new failed");
    MiningMessages::dispatch(frame_view).expect("BUG: pool_recv dispatch failed")
}

/// Read one SV2 frame from `reader` and decode it with `codec`, returning
/// the header and the plaintext without dispatching it, for frames of an
/// extension the mining message group does not know.
async fn pool_recv_raw(
    codec: &mut EncryptedCodec,
    reader: &mut tokio::io::DuplexStream,
) -> (Header, Vec<u8>) {
    let mut header_bytes = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
    reader
        .read_exact(&mut header_bytes)
        .await
        .expect("BUG: pool_recv_raw header read_exact failed");
    let header = codec
        .decode_header(&mut header_bytes)
        .expect("BUG: pool_recv_raw decode_header failed");

    let payload_wire_size = EncryptedCodec::payload_wire_size(header.msg_length as usize);
    let mut payload = vec![0u8; payload_wire_size];
    reader
        .read_exact(&mut payload)
        .await
        .expect("BUG: pool_recv_raw payload read_exact failed");
    codec
        .decode_frame(header, &mut payload)
        .expect("BUG: pool_recv_raw decode_frame failed");
    payload.truncate(header.msg_length as usize);
    (header, payload)
}

// ── Slow / broken transports for Group 3 tests ───────────────────────────────────

/// Number of bytes this transport delivers per `read`/`write` call.
const SLOW_CHUNK_BYTES: usize = 1;

/// Transport that delivers at most `SLOW_CHUNK_BYTES` bytes per `read` or
/// `write` call. Used to exercise partial-read and partial-write resumption.
struct SlowTransport {
    readable: Vec<u8>,
    read_pos: usize,
    written: Vec<u8>,
}

impl SlowTransport {
    fn new(readable: Vec<u8>) -> Self {
        Self {
            readable,
            read_pos: 0,
            written: Vec::new(),
        }
    }
}

impl ErrorType for SlowTransport {
    type Error = core::convert::Infallible;
}

impl Read for SlowTransport {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let available = self.readable.len() - self.read_pos;
        if available == 0 {
            return Ok(0);
        }
        let count = buffer.len().min(SLOW_CHUNK_BYTES).min(available);
        buffer[..count].copy_from_slice(&self.readable[self.read_pos..self.read_pos + count]);
        self.read_pos += count;
        Ok(count)
    }
}

impl Write for SlowTransport {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let count = buffer.len().min(SLOW_CHUNK_BYTES);
        self.written.extend_from_slice(&buffer[..count]);
        Ok(count)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Transport whose reads return `Ok(0)` unconditionally, simulating a
/// closed connection. Writes consume bytes normally so `connect` can flush
/// before the read-side goes EOF.
struct EofTransport {
    written: Vec<u8>,
}

impl EofTransport {
    fn new() -> Self {
        Self {
            written: Vec::new(),
        }
    }
}

impl ErrorType for EofTransport {
    type Error = core::convert::Infallible;
}

impl Read for EofTransport {
    async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}

impl Write for EofTransport {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.written.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Transport that delivers one chunk of bytes on its first `read`, then parks
/// forever on the next `read`. Used to drive a `drive` future to the point
/// where it suspends mid-frame so the test can drop and resume it.
struct PendOnceTransport {
    chunk: Vec<u8>,
    delivered: bool,
}

impl PendOnceTransport {
    fn new(chunk: Vec<u8>) -> Self {
        Self {
            chunk,
            delivered: false,
        }
    }
}

impl ErrorType for PendOnceTransport {
    type Error = core::convert::Infallible;
}

impl Read for PendOnceTransport {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        if self.delivered {
            // Park forever. The test polls this future once, observes
            // Pending, and drops it without ever re-polling, so the waker
            // is never needed.
            core::future::pending::<()>().await;
        }
        self.delivered = true;
        let count = buffer.len().min(self.chunk.len());
        buffer[..count].copy_from_slice(&self.chunk[..count]);
        Ok(count)
    }
}

impl Write for PendOnceTransport {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Transport whose writes always return `Ok(0)`, simulating a full
/// send buffer. Reads also return `Ok(0)`.
struct ZeroWriteTransport;

impl ErrorType for ZeroWriteTransport {
    type Error = core::convert::Infallible;
}

impl Read for ZeroWriteTransport {
    async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}

impl Write for ZeroWriteTransport {
    async fn write(&mut self, _buffer: &[u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// ── Default config builder ────────────────────────────────────────────────────────

/// Build a default `Sv2Config` with `pool_pubkey = None` (`ValidateNothing`).
fn default_config() -> Sv2Config {
    Sv2Config::new("test.worker", None).expect("BUG: worker name fits in 128 bytes")
}

// ── Fake-pool session fixture ─────────────────────────────────────────────────────

/// Accepted share count used in `SubmitSharesSuccess`.
const FAKE_ACCEPTED_COUNT: u32 = 3;

/// Shares sum used in `SubmitSharesSuccess`.
const FAKE_SHARES_SUM: u64 = 0x0000_0000_0000_CAFE;

/// Test fixture owning both ends of an in-memory SV2 session.
///
/// The client end wraps a `tokio::io::DuplexStream` in `FromTokio`.
/// The pool end holds the raw `DuplexStream` plus an `EncryptedCodec`
/// produced by the noise `accept` handshake. After `Session::new()`,
/// the client has completed `connect` and the pool queue has been
/// drained of the two initial frames (`SetupConnection` +
/// `OpenStandardMiningChannel`).
struct Session {
    client: StratumV2Client<'static, OsRng, secp256k1::All>,
    client_transport: FromTokio<tokio::io::DuplexStream>,
    pool_codec: EncryptedCodec,
    pool_end: tokio::io::DuplexStream,
}

impl Session {
    /// Perform the full SV2 noise handshake and drain the initial
    /// `SetupConnection` + `OpenStandardMiningChannel` frames from the
    /// pool queue. The returned `Session` is ready for pool messages.
    async fn new() -> Self {
        Self::with_config(default_config()).await
    }

    /// Like `new` but uses an explicit `Sv2Config`.
    async fn with_config(config: Sv2Config) -> Self {
        let config: &'static Sv2Config = Box::leak(Box::new(config));
        let cert = make_server_cert();
        let (client_duplex, pool_duplex) = tokio::io::duplex(8192);

        let mut client = StratumV2Client::new(config, crypto_ctx(), OsRng);
        let mut client_transport = FromTokio::new(client_duplex);

        // The accept arm owns `pool_duplex` and returns it alongside the
        // codec so the drain step can use it after the join completes. The
        // inner block scopes the `FromTokio` borrow so it ends before the
        // duplex is moved out of the arm.
        let (connect_result, (pool_codec, mut pool_end)) =
            tokio::join!(client.connect(&mut client_transport), async move {
                let mut pool_duplex = pool_duplex;
                let session: EstablishedSession = {
                    let mut wrapped = FromTokio::new(&mut pool_duplex);
                    accept(&mut wrapped, OsRng, crypto_ctx(), &cert)
                        .await
                        .expect("BUG: fake-pool accept failed")
                };
                let (codec, _session_id) = EncryptedCodec::from_handshake(session);
                (codec, pool_duplex)
            },);
        connect_result.expect("BUG: Session::with_config connect failed");
        let mut pool_codec = pool_codec;

        // Drain SetupConnection + OpenStandardMiningChannel.
        let _setup = pool_recv(&mut pool_codec, &mut pool_end).await;
        let _open = pool_recv(&mut pool_codec, &mut pool_end).await;

        Self {
            client,
            client_transport,
            pool_codec,
            pool_end,
        }
    }

    /// Encode `message` with the pool codec and write it to the client.
    async fn pool_send_msg<F: ii_sv2::framing::IntoFrame>(&mut self, message: &F) {
        pool_send(&mut self.pool_codec, message, &mut self.pool_end).await;
    }

    /// Drive one frame on the client, then call `accept` with `visitor`.
    /// Panics on `Err`.
    async fn drive_and_accept(&mut self, visitor: &mut RecordingVisitor) {
        self.client
            .drive(&mut self.client_transport)
            .await
            .expect("BUG: drive_and_accept drive failed");
        self.client.accept(visitor);
    }

    /// Drive one frame and expect an error. Panics if drive returns `Ok`.
    async fn drive_expect_err(&mut self) -> Error {
        self.client
            .drive(&mut self.client_transport)
            .await
            .expect_err("BUG: drive_expect_err expected Err but got Ok")
    }

    /// Receive one frame from the client on the pool side.
    async fn pool_recv_msg(&mut self) -> MiningMessages<'static> {
        pool_recv(&mut self.pool_codec, &mut self.pool_end).await
    }

    /// Send `OpenStandardMiningChannelSuccess` and drain the visitor
    /// callbacks, leaving the channel in the open state.
    async fn open_channel(&mut self, visitor: &mut RecordingVisitor) {
        let message = OpenStandardMiningChannelSuccess {
            request_id: 1,
            channel_id: FAKE_CHANNEL_ID,
            target: Uint256::from_le_bytes(&FAKE_TARGET),
            extranonce_prefix: Bytes0_32::try_from(FAKE_EXTRANONCE_PREFIX.as_slice())
                .expect("BUG: FAKE_EXTRANONCE_PREFIX fits in Bytes0_32"),
            group_channel_id: 0,
        };
        self.pool_send_msg(&message).await;
        self.drive_and_accept(visitor).await;
    }

    /// Send `SetNewPrevHash` (with a matching future `NewMiningJob`) and
    /// drain the callbacks, leaving the session with a live job ready to
    /// submit. Returns the activated `JobId`.
    async fn set_up_job(&mut self, visitor: &mut RecordingVisitor) -> JobId {
        let future_job = NewMiningJob {
            channel_id: FAKE_CHANNEL_ID,
            job_id: FAKE_JOB_ID,
            min_ntime: None,
            version: 0x2000_0000,
            merkle_root: Uint256::from_le_bytes(&FAKE_MERKLE_ROOT),
        };
        self.pool_send_msg(&future_job).await;
        self.drive_and_accept(visitor).await;

        let prev_hash = SetNewPrevHash {
            channel_id: FAKE_CHANNEL_ID,
            job_id: FAKE_JOB_ID,
            prev_hash: Uint256::from_le_bytes(&FAKE_PREV_HASH),
            min_ntime: FAKE_NTIME,
            nbits: FAKE_NBITS,
        };
        self.pool_send_msg(&prev_hash).await;
        self.drive_and_accept(visitor).await;

        let job_count = visitor.jobs.len();
        assert!(
            job_count > 0,
            "BUG: set_up_job must produce at least one job callback"
        );
        visitor.jobs[job_count - 1].id
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 1 — pure guards (no handshake)
// ─────────────────────────────────────────────────────────────────────────────

/// `submit` before `connect` returns `Error::Adapter` because the
/// session is in `PreHandshake` state with no channel or job state.
#[tokio::test]
async fn submit_before_connect_returns_adapter_error() {
    let config = default_config();
    let mut client = StratumV2Client::new(&config, crypto_ctx(), OsRng);
    let share = Share {
        id: JobId(0),
        nonce: 0,
        ntime: 0,
        version_bits: 0,
        extranonce_2: &[],
    };
    assert_eq!(
        client.submit(&share),
        Err(Error::Adapter),
        "submit before connect must return Adapter",
    );
}

/// `drive` before `connect` returns `Error::Adapter` because the
/// session is in `PreHandshake` state.
#[tokio::test]
async fn drive_before_connect_returns_adapter_error() {
    let config = default_config();
    let mut client = StratumV2Client::new(&config, crypto_ctx(), OsRng);
    let mut transport = EofTransport::new();
    assert_eq!(
        client.drive(&mut transport).await,
        Err(Error::Adapter),
        "drive before connect must return Adapter",
    );
}

/// `accept` with nothing buffered is a no-op: the visitor receives no
/// callbacks because `FrameState` starts at `ReadingHeader { received: 0 }`.
#[tokio::test]
async fn accept_with_nothing_buffered_is_a_no_op() {
    let config = default_config();
    let mut client = StratumV2Client::new(&config, crypto_ctx(), OsRng);
    let mut visitor = RecordingVisitor::default();
    client.accept(&mut visitor);
    assert!(
        visitor.session_updates.is_empty(),
        "accept with no buffered frame must not fire session_update",
    );
    assert!(
        visitor.jobs.is_empty(),
        "accept with no buffered frame must not fire visit_new_job",
    );
    assert!(
        visitor.submit_results.is_empty(),
        "accept with no buffered frame must not fire visit_submit_result",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 2 — handshake + dispatch
// ─────────────────────────────────────────────────────────────────────────────

/// `connect` completes successfully. The pool side reads a
/// `SetupConnection` (protocol=0, version=2) followed by an
/// `OpenStandardMiningChannel` (`request_id=1`).
#[tokio::test]
async fn connect_sends_setup_connection_then_open_channel() {
    let config = default_config();
    let cert = make_server_cert();
    let (client_duplex, pool_duplex) = tokio::io::duplex(8192);

    let mut client = StratumV2Client::new(&config, crypto_ctx(), OsRng);
    let mut client_transport = FromTokio::new(client_duplex);

    // The accept arm owns `pool_duplex` and returns it alongside the codec.
    // The inner block scopes the `FromTokio` borrow so it ends before the
    // duplex is moved out of the arm.
    let (connect_result, (pool_codec, mut pool_end)) =
        tokio::join!(client.connect(&mut client_transport), async move {
            let mut pool_duplex = pool_duplex;
            let session: EstablishedSession = {
                let mut wrapped = FromTokio::new(&mut pool_duplex);
                accept(&mut wrapped, OsRng, crypto_ctx(), &cert)
                    .await
                    .expect("BUG: fake-pool accept failed")
            };
            let (codec, _session_id) = EncryptedCodec::from_handshake(session);
            (codec, pool_duplex)
        },);
    connect_result.expect("BUG: connect must succeed against a valid responder");
    let mut pool_codec = pool_codec;

    let setup = pool_recv(&mut pool_codec, &mut pool_end).await;
    match setup {
        MiningMessages::SetupConnection(message) => {
            assert_eq!(
                message.protocol, 0,
                "setup-connection protocol must be 0 (mining)"
            );
            assert_eq!(message.min_version, 2, "min_version must be 2");
            assert_eq!(message.max_version, 2, "max_version must be 2");
        }
        other => panic!("expected SetupConnection, got {other:?}"),
    }

    let open = pool_recv(&mut pool_codec, &mut pool_end).await;
    match open {
        MiningMessages::OpenStandardMiningChannel(message) => {
            assert_eq!(message.request_id, 1, "open-channel request_id must be 1");
        }
        other => panic!("expected OpenStandardMiningChannel, got {other:?}"),
    }
}

/// `SetupConnectionSuccess` whose flags do not pin the version →
/// visitor sees `SessionUpdate::SetVersionMask(0x1FFF_E000)`. The
/// success flags carry the upstream's requirements, so any flags
/// without `REQUIRES_FIXED_VERSION` leave the version free to roll.
#[tokio::test]
async fn setup_connection_success_with_version_rolling_flag_sets_mask() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = SetupConnectionSuccess {
        used_version: 2,
        // Every success-space flag set except the one that pins the
        // version, so the test exercises the mask decision rather than
        // a value whose low bit happens to be clear.
        flags: !SetupConnectionSuccess::REQUIRES_FIXED_VERSION,
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    let mask = visitor
        .session_updates
        .iter()
        .find_map(|update| match update {
            RecordedUpdate::VersionMask(mask) => Some(*mask),
            _ => None,
        })
        .expect("BUG: expected SetVersionMask callback after SetupConnectionSuccess");
    assert_eq!(
        mask, 0x1FFF_E000,
        "version rolling mask must be BIP320 bits 13-28"
    );
}

/// `SetupConnectionSuccess` requiring a fixed version → no mask reaches
/// the visitor and a subsequent `drive` returns
/// `Error::SessionRejected(SessionRejection::VersionRollingRefused)`.
#[tokio::test]
async fn setup_connection_success_requiring_a_fixed_version_rejects_session() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = SetupConnectionSuccess {
        used_version: 2,
        flags: SetupConnectionSuccess::REQUIRES_FIXED_VERSION,
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    assert!(
        !visitor
            .session_updates
            .iter()
            .any(|update| matches!(update, RecordedUpdate::VersionMask(_))),
        "a pinned version must not publish a mask"
    );
    let error = session.drive_expect_err().await;
    assert_eq!(
        error,
        Error::SessionRejected(SessionRejection::VersionRollingRefused),
        "drive after a fixed-version setup must return SessionRejected(VersionRollingRefused)",
    );
}

/// `OpenStandardMiningChannelSuccess` → visitor sees `SetTarget` then
/// `SetExtranonce`. Afterwards `submit` succeeds (proving `channel_id` was
/// recorded). The test also exercises the full job path
/// (future job → `SetNewPrevHash` → clean job → submit).
#[tokio::test]
async fn open_channel_success_fires_set_target_and_set_extranonce_then_submit_works() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;

    assert_eq!(
        visitor.session_updates.len(),
        2,
        "channel-open must fire exactly SetTarget + SetExtranonce, got {}",
        visitor.session_updates.len(),
    );
    // Verify the target value and extranonce prefix were delivered correctly.
    match &visitor.session_updates[0] {
        RecordedUpdate::Target(target) => {
            assert_eq!(
                target, &FAKE_TARGET,
                "first update must be SetTarget with the pool's target",
            );
        }
        other => panic!("expected Target update, got {other:?}"),
    }
    match &visitor.session_updates[1] {
        RecordedUpdate::Extranonce { prefix, en2_size } => {
            assert_eq!(
                prefix.as_slice(),
                FAKE_EXTRANONCE_PREFIX.as_slice(),
                "second update must be SetExtranonce with the pool's prefix",
            );
            assert_eq!(*en2_size, 0, "V2 standard channels have en2_size = 0");
        }
        other => panic!("expected Extranonce update, got {other:?}"),
    }

    // Get a job and confirm submit no longer returns Adapter.
    let job_id = session.set_up_job(&mut visitor).await;
    let share = Share {
        id: job_id,
        nonce: 0xDEAD_BEEF,
        ntime: FAKE_NTIME,
        version_bits: 0,
        extranonce_2: &[],
    };
    assert_eq!(
        session.client.submit(&share),
        Ok(()),
        "submit must succeed after channel open and job received",
    );
}

/// `SetupConnectionError` → a subsequent `drive` returns
/// `Error::SessionRejected(SessionRejection::SetupConnectionError)`.
#[tokio::test]
async fn setup_connection_error_rejects_session() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = SetupConnectionError {
        flags: 0,
        code: Str0_255::try_from("invalid-flags").expect("BUG: static string fits in Str0_255"),
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    // A subsequent drive: the session is now Rejected, but we need to send
    // something so drive has bytes to read. With a Rejected state, drive
    // returns immediately without reading.
    let error = session.drive_expect_err().await;
    assert_eq!(
        error,
        Error::SessionRejected(SessionRejection::SetupConnectionError),
        "drive after setup-connection-error must return SessionRejected(SetupConnectionError)",
    );
}

/// `OpenMiningChannelError` → a subsequent `drive` returns
/// `Error::SessionRejected(SessionRejection::OpenMiningChannelError)`.
#[tokio::test]
async fn open_mining_channel_error_rejects_session() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = OpenMiningChannelError {
        request_id: 1,
        code: Str0_255::try_from(OpenMiningChannelError::UNKNOWN_USER)
            .expect("BUG: constant string fits in Str0_255"),
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    let error = session.drive_expect_err().await;
    assert_eq!(
        error,
        Error::SessionRejected(SessionRejection::OpenMiningChannelError),
        "drive after open-mining-channel-error must return SessionRejected(OpenMiningChannelError)",
    );
}

/// A future `NewMiningJob` (`min_ntime = None`) followed by a matching
/// `SetNewPrevHash` → visitor sees `visit_new_job` with `clean_jobs = true`.
#[tokio::test]
async fn future_job_then_set_new_prev_hash_fires_clean_job() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    visitor.session_updates.clear();

    let future_job = NewMiningJob {
        channel_id: FAKE_CHANNEL_ID,
        job_id: FAKE_JOB_ID,
        min_ntime: None,
        version: 0x2000_0000,
        merkle_root: Uint256::from_le_bytes(&FAKE_MERKLE_ROOT),
    };
    session.pool_send_msg(&future_job).await;
    session.drive_and_accept(&mut visitor).await;

    // No job callback yet — activation waits for SetNewPrevHash.
    assert!(
        visitor.jobs.is_empty(),
        "future job must not emit a job callback before SetNewPrevHash",
    );

    let prev_hash = SetNewPrevHash {
        channel_id: FAKE_CHANNEL_ID,
        job_id: FAKE_JOB_ID,
        prev_hash: Uint256::from_le_bytes(&FAKE_PREV_HASH),
        min_ntime: FAKE_NTIME,
        nbits: FAKE_NBITS,
    };
    session.pool_send_msg(&prev_hash).await;
    session.drive_and_accept(&mut visitor).await;

    assert_eq!(
        visitor.jobs.len(),
        1,
        "SetNewPrevHash must activate the pending future job",
    );
    assert!(
        visitor.jobs[0].clean_jobs,
        "job activated by SetNewPrevHash must have clean_jobs = true",
    );
}

/// An immediate `NewMiningJob` (`min_ntime = Some`) received AFTER a
/// `SetNewPrevHash` has established the prev-hash → `visit_new_job`
/// fires with `clean_jobs = false`.
#[tokio::test]
async fn immediate_job_after_prev_hash_fires_non_clean_job() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    visitor.session_updates.clear();

    // Establish prev-hash via a dummy future job.
    let dummy_future_job = NewMiningJob {
        channel_id: FAKE_CHANNEL_ID,
        job_id: 99,
        min_ntime: None,
        version: 0x2000_0000,
        merkle_root: Uint256::from_le_bytes(&FAKE_MERKLE_ROOT),
    };
    session.pool_send_msg(&dummy_future_job).await;
    session.drive_and_accept(&mut visitor).await;

    let prev_hash = SetNewPrevHash {
        channel_id: FAKE_CHANNEL_ID,
        job_id: 99,
        prev_hash: Uint256::from_le_bytes(&FAKE_PREV_HASH),
        min_ntime: FAKE_NTIME,
        nbits: FAKE_NBITS,
    };
    session.pool_send_msg(&prev_hash).await;
    session.drive_and_accept(&mut visitor).await;

    // Clear the clean-job from the future-job activation.
    visitor.jobs.clear();

    // Now send an immediate job (min_ntime = Some).
    let immediate_job = NewMiningJob {
        channel_id: FAKE_CHANNEL_ID,
        job_id: FAKE_JOB_ID,
        min_ntime: Some(FAKE_NTIME),
        version: 0x2000_0000,
        merkle_root: Uint256::from_le_bytes(&FAKE_MERKLE_ROOT),
    };
    session.pool_send_msg(&immediate_job).await;
    session.drive_and_accept(&mut visitor).await;

    assert_eq!(
        visitor.jobs.len(),
        1,
        "immediate job after prev-hash must produce exactly one job callback",
    );
    assert!(
        !visitor.jobs[0].clean_jobs,
        "immediate job (min_ntime = Some) must have clean_jobs = false",
    );
}

/// An immediate `NewMiningJob` (`min_ntime = Some`) received BEFORE any
/// `SetNewPrevHash` → job is silently dropped (no visitor callback).
#[tokio::test]
async fn immediate_job_before_prev_hash_is_dropped() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    visitor.session_updates.clear();

    // No SetNewPrevHash sent yet.
    let immediate_job = NewMiningJob {
        channel_id: FAKE_CHANNEL_ID,
        job_id: FAKE_JOB_ID,
        min_ntime: Some(FAKE_NTIME),
        version: 0x2000_0000,
        merkle_root: Uint256::from_le_bytes(&FAKE_MERKLE_ROOT),
    };
    session.pool_send_msg(&immediate_job).await;
    session.drive_and_accept(&mut visitor).await;

    assert!(
        visitor.jobs.is_empty(),
        "immediate job before any prev_hash must be silently dropped",
    );
}

/// `SubmitSharesSuccess` → visitor sees `SubmitResult::Accepted` with
/// the correct `count` and `work` values.
#[tokio::test]
async fn submit_shares_success_fires_accepted_result() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = SubmitSharesSuccess {
        channel_id: FAKE_CHANNEL_ID,
        last_sequence_number: 0,
        new_submits_accepted_count: FAKE_ACCEPTED_COUNT,
        new_shares_sum: FAKE_SHARES_SUM,
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    assert_eq!(
        visitor.submit_results.len(),
        1,
        "SubmitSharesSuccess must fire exactly one submit_result callback",
    );
    match &visitor.submit_results[0] {
        RecordedSubmitResult::Accepted { count, work } => {
            assert_eq!(*count, FAKE_ACCEPTED_COUNT, "accepted count must match");
            assert_eq!(*work, FAKE_SHARES_SUM, "shares sum must match");
        }
        RecordedSubmitResult::Rejected { .. } => panic!("expected Accepted, got Rejected"),
    }
}

/// `SubmitSharesError` → visitor sees `SubmitResult::Rejected` with
/// the correct error message.
#[tokio::test]
async fn submit_shares_error_fires_rejected_result() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = SubmitSharesError {
        channel_id: FAKE_CHANNEL_ID,
        sequence_number: 0,
        error_code: Str0_255::try_from(SubmitSharesError::STALE_SHARE)
            .expect("BUG: STALE_SHARE constant fits in Str0_255"),
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    assert_eq!(
        visitor.submit_results.len(),
        1,
        "SubmitSharesError must fire exactly one submit_result callback",
    );
    match &visitor.submit_results[0] {
        RecordedSubmitResult::Rejected { error_message } => {
            assert_eq!(
                error_message.as_deref(),
                Some(SubmitSharesError::STALE_SHARE),
                "error message must match the wire error code",
            );
        }
        RecordedSubmitResult::Accepted { .. } => panic!("expected Rejected, got Accepted"),
    }
}

/// `submit` after channel open queues a frame. The next `drive` flushes it.
/// The pool side decodes a `SubmitSharesStandard` with the expected
/// `channel_id`, `sequence_number = 0`, and `job_id`.
///
/// The `drive` call flushes outbound bytes first, then reads an inbound frame.
/// We send a `SubmitSharesSuccess` from the pool after queuing the submit so
/// `drive` has something to read after flushing.
#[tokio::test]
async fn submit_after_channel_open_flushes_on_drive() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    visitor.session_updates.clear();

    let job_id = session.set_up_job(&mut visitor).await;

    let share = Share {
        id: job_id,
        nonce: 0xDEAD_BEEF,
        ntime: FAKE_NTIME,
        version_bits: 0,
        extranonce_2: &[],
    };
    session
        .client
        .submit(&share)
        .expect("BUG: submit must succeed with valid job_id and open channel");

    // Send a SubmitSharesSuccess so drive has an inbound frame to read
    // after flushing the queued submit.
    let success = SubmitSharesSuccess {
        channel_id: FAKE_CHANNEL_ID,
        last_sequence_number: 0,
        new_submits_accepted_count: 1,
        new_shares_sum: 1,
    };
    session.pool_send_msg(&success).await;

    session.drive_and_accept(&mut visitor).await;

    // The pool must have received the SubmitSharesStandard before the
    // success reply was queued. It was written to the pool end during
    // drive's flush phase; now read it from the pool side.
    let submitted = session.pool_recv_msg().await;
    match submitted {
        MiningMessages::SubmitSharesStandard(message) => {
            assert_eq!(
                message.channel_id, FAKE_CHANNEL_ID,
                "submitted channel_id must match the opened channel",
            );
            assert_eq!(
                message.sequence_number, 0,
                "first submit must have sequence_number 0",
            );
            assert_eq!(
                message.job_id, FAKE_JOB_ID,
                "submitted job_id must match the wire job_id",
            );
            assert_eq!(
                message.version, 0x2000_0000,
                "with no rolled bits the wire version is the job base version",
            );
        }
        other => panic!("expected SubmitSharesStandard, got {other:?}"),
    }
}

/// A share carrying rolled BIP320 bits must reach the wire as the full
/// `nVersion`: the job's base version with the rolled bits OR'd into the
/// BIP320 field. The chip hashes this full version, so the pool can only
/// reproduce the share's hash when the submit carries it whole.
#[tokio::test]
async fn submit_reconstructs_full_version_from_rolled_bits() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    visitor.session_updates.clear();

    let job_id = session.set_up_job(&mut visitor).await;

    // 0x48 << 13 = 0x0009_0000 — only the BIP320 rolled bits, exactly
    // what the chip reports as `version_bits`.
    let share = Share {
        id: job_id,
        nonce: 0xDEAD_BEEF,
        ntime: FAKE_NTIME,
        version_bits: 0x0009_0000,
        extranonce_2: &[],
    };
    session
        .client
        .submit(&share)
        .expect("BUG: submit must succeed with valid job_id and open channel");

    let success = SubmitSharesSuccess {
        channel_id: FAKE_CHANNEL_ID,
        last_sequence_number: 0,
        new_submits_accepted_count: 1,
        new_shares_sum: 1,
    };
    session.pool_send_msg(&success).await;
    session.drive_and_accept(&mut visitor).await;

    match session.pool_recv_msg().await {
        MiningMessages::SubmitSharesStandard(message) => {
            assert_eq!(
                message.version, 0x2009_0000,
                "wire version must be job base (0x2000_0000) OR rolled bits (0x0009_0000)",
            );
        }
        other => panic!("expected SubmitSharesStandard, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Oversized extranonce note
// ─────────────────────────────────────────────────────────────────────────────
//
// The `ExtranonceTooLarge` rejection branch in `accept` checks
// `prefix_len > MAX_EXTRANONCE_PREFIX` (i.e. > 32). The wire type
// `Bytes0_32` permits at most 32 bytes, so `prefix_len` is always ≤ 32.
// This means the branch is unreachable via `OpenStandardMiningChannelSuccess`
// with the current wire types. A future wire-type change (e.g. Bytes0_64)
// would make it reachable. The test below confirms that a 32-byte prefix
// (the wire-type maximum) is accepted, not rejected.

/// A 32-byte extranonce prefix (at the wire-type maximum of `Bytes0_32`)
/// is accepted by the client without rejecting the session.
#[tokio::test]
async fn max_extranonce_prefix_at_wire_type_boundary_is_accepted() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let max_prefix = [0xABu8; 32];
    let message = OpenStandardMiningChannelSuccess {
        request_id: 1,
        channel_id: FAKE_CHANNEL_ID,
        target: Uint256::from_le_bytes(&FAKE_TARGET),
        extranonce_prefix: Bytes0_32::try_from(max_prefix.as_slice())
            .expect("BUG: 32-byte slice fits in Bytes0_32"),
        group_channel_id: 0,
    };
    session.pool_send_msg(&message).await;
    session.drive_and_accept(&mut visitor).await;

    // Session must remain live: send another frame and drive successfully.
    let dummy = SetupConnectionSuccess {
        used_version: 2,
        flags: 0,
    };
    session.pool_send_msg(&dummy).await;
    let result = session.client.drive(&mut session.client_transport).await;
    assert!(
        result.is_ok(),
        "session must remain live after a 32-byte extranonce prefix",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 3 — transport / error paths
// ─────────────────────────────────────────────────────────────────────────────

/// A read returning `Ok(0)` → `drive` returns `Error::Transport`.
/// The next `drive` returns `Error::Decode` without reading (frame state
/// is poisoned to `Broken`).
#[tokio::test]
async fn eof_read_returns_transport_then_decode_on_retry() {
    let mut session = Session::new().await;

    // The client is connected; use an EofTransport for subsequent drives.
    // The AEAD chain in the client is not affected — we're just changing
    // the underlying byte source.
    let mut eof = EofTransport::new();

    let first_error = session
        .client
        .drive(&mut eof)
        .await
        .expect_err("BUG: drive over EofTransport must fail");
    assert_eq!(
        first_error,
        Error::Transport,
        "first drive over EOF transport must return Transport",
    );

    let second_error = session
        .client
        .drive(&mut eof)
        .await
        .expect_err("BUG: second drive over EofTransport must fail");
    assert_eq!(
        second_error,
        Error::Decode,
        "second drive after frame-state poisoned must return Decode",
    );
}

/// A write returning `Ok(0)` while flushing a queued submit →
/// `Error::Transport`.
#[tokio::test]
async fn zero_write_during_flush_returns_transport_error() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    let job_id = session.set_up_job(&mut visitor).await;

    let share = Share {
        id: job_id,
        nonce: 0xCAFE,
        ntime: FAKE_NTIME,
        version_bits: 0,
        extranonce_2: &[],
    };
    session
        .client
        .submit(&share)
        .expect("BUG: submit must succeed");

    // Drive with a ZeroWriteTransport; the flush returns Ok(0) on the
    // first write attempt, triggering Transport.
    let mut zero_writer = ZeroWriteTransport;
    let error = session
        .client
        .drive(&mut zero_writer)
        .await
        .expect_err("BUG: drive with ZeroWriteTransport must fail");
    assert_eq!(
        error,
        Error::Transport,
        "drive with write returning Ok(0) must return Transport",
    );
}

/// Partial reads (1 byte at a time) still assemble a full encrypted frame
/// correctly. The header-accumulation loop and payload-accumulation loop
/// must both handle short reads without losing bytes.
///
/// Strategy: encode a pool→client frame using the pool's `EncryptedCodec`
/// (whose AEAD chain is synchronised with the client's), then feed those
/// bytes through a `SlowTransport` to the client's `drive`. If the loops
/// are correct, the decrypted frame dispatches to the expected visitor
/// callback.
#[tokio::test]
async fn partial_reads_assemble_full_frame() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let message = SetupConnectionSuccess {
        used_version: 2,
        flags: 0,
    };
    let mut wire_buffer = [0u8; POOL_WIRE_BUFFER_SIZE];
    let wire_len = session
        .pool_codec
        .encode(&message, &mut wire_buffer)
        .expect("BUG: pool encode failed");
    let frame_bytes: Vec<u8> = wire_buffer[..wire_len].to_vec();

    let mut slow = SlowTransport::new(frame_bytes);
    session
        .client
        .drive(&mut slow)
        .await
        .expect("BUG: drive over SlowTransport must succeed");
    session.client.accept(&mut visitor);

    assert_eq!(
        visitor.session_updates.len(),
        1,
        "SetupConnectionSuccess via byte-by-byte reads must produce one session update",
    );
    assert!(
        matches!(
            visitor.session_updates[0],
            RecordedUpdate::VersionMask(0x1FFF_E000)
        ),
        "SetupConnectionSuccess flags=0 leaves the version free to roll",
    );
}

/// Byte offset at which the cancellation test splits the frame. It lands
/// inside the 22-byte encrypted header so the cancelled future parks while
/// still accumulating the header.
const MID_HEADER_SPLIT: usize = 10;

/// A `drive` future cancelled (dropped) mid-frame resumes correctly on the
/// next `drive`. The receive `FrameState` and the partially filled header
/// buffer live on the client, not in the future, so dropping a future that
/// has read only part of the header preserves those bytes; a second `drive`
/// over the remaining bytes completes the same frame. This exercises the
/// cancellation-safety invariant the receive state machine documents.
#[tokio::test]
async fn drive_future_cancelled_mid_frame_resumes() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    // Encode one pool->client frame whose AEAD chain is synced with the client.
    let message = SetupConnectionSuccess {
        used_version: 2,
        flags: 0,
    };
    let mut wire_buffer = [0u8; POOL_WIRE_BUFFER_SIZE];
    let wire_len = session
        .pool_codec
        .encode(&message, &mut wire_buffer)
        .expect("BUG: pool encode failed");
    let frame_bytes: Vec<u8> = wire_buffer[..wire_len].to_vec();
    // The split must land inside the header so the future parks in
    // ReadingHeader rather than completing the header in one read.
    const { assert!(MID_HEADER_SPLIT < EncryptedCodec::HEADER_WIRE_SIZE) };

    // Poll a drive future once over a transport that delivers MID_HEADER_SPLIT
    // bytes then parks forever. The future must report Pending; then it is
    // dropped, cancelling the read with those header bytes already on the
    // client.
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    {
        let mut pend = PendOnceTransport::new(frame_bytes[..MID_HEADER_SPLIT].to_vec());
        let drive_future = core::pin::pin!(session.client.drive(&mut pend));
        match drive_future.poll(&mut context) {
            Poll::Pending => {}
            Poll::Ready(result) => {
                panic!("BUG: drive must park mid-frame, got Ready({result:?})")
            }
        }
        // `drive_future` and `pend` drop here, cancelling the future.
    }

    // Resume: feed the remaining bytes through a fresh transport. The client
    // must pick up from the persisted ReadingHeader state.
    let mut resume = SlowTransport::new(frame_bytes[MID_HEADER_SPLIT..].to_vec());
    session
        .client
        .drive(&mut resume)
        .await
        .expect("BUG: resumed drive must complete the frame");
    session.client.accept(&mut visitor);

    assert_eq!(
        visitor.session_updates.len(),
        1,
        "the resumed frame must dispatch exactly one session update",
    );
    assert!(
        matches!(
            visitor.session_updates[0],
            RecordedUpdate::VersionMask(0x1FFF_E000)
        ),
        "resumed SetupConnectionSuccess(flags=0) leaves the version free to roll",
    );
}

/// Partial outbound writes (1 byte at a time) flush the queued submit
/// correctly. After all bytes are written, the submit frame bytes are
/// present in `SlowTransport::written`.
///
/// Note: the oversized-payload branch (`payload_total > WIRE_BUFFER_BYTES`
/// in `drive`) is NOT reachable with real standard-channel messages because
/// all such messages encode to well under the 1 KiB wire buffer. The
/// `Broken`/`Decode` transition from a corrupt payload (tested below)
/// covers the poisoned-state path instead.
#[tokio::test]
async fn partial_outbound_writes_flush_correctly() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.open_channel(&mut visitor).await;
    let job_id = session.set_up_job(&mut visitor).await;

    let share = Share {
        id: job_id,
        nonce: 0x1234,
        ntime: FAKE_NTIME,
        version_bits: 0,
        extranonce_2: &[],
    };
    session
        .client
        .submit(&share)
        .expect("BUG: submit must succeed");

    // Build inbound bytes (SetupConnectionSuccess) for drive to read
    // AFTER flushing the submit. Drive flushes outbound first, then reads.
    let inbound_message = SetupConnectionSuccess {
        used_version: 2,
        flags: 0,
    };
    let mut wire_buffer = [0u8; POOL_WIRE_BUFFER_SIZE];
    let inbound_len = session
        .pool_codec
        .encode(&inbound_message, &mut wire_buffer)
        .expect("BUG: pool encode of inbound frame failed");
    let inbound_bytes: Vec<u8> = wire_buffer[..inbound_len].to_vec();

    let mut slow = SlowTransport::new(inbound_bytes);
    session
        .client
        .drive(&mut slow)
        .await
        .expect("BUG: drive with slow transport must succeed");

    // `SubmitSharesStandard` has 6 × u32 = 24 bytes body; wire frame is
    // HEADER_WIRE_SIZE + body + MAC = 22 + 24 + 16 = 62 bytes.
    let submit_body_bytes: usize = 24;
    let mac_bytes: usize = 16;
    let expected_submit_wire_len = EncryptedCodec::HEADER_WIRE_SIZE + submit_body_bytes + mac_bytes;
    assert!(
        slow.written.len() >= expected_submit_wire_len,
        "slow transport must hold at least {} bytes for the submit frame, got {}",
        expected_submit_wire_len,
        slow.written.len(),
    );
}

/// A corrupted byte in the encrypted payload → `drive` returns
/// `Error::Decode`. The next `drive` also returns `Error::Decode`
/// without reading from the transport (frame state is poisoned to `Broken`).
///
/// Note: the `payload_total > WIRE_BUFFER_BYTES` (1024) branch in `drive`
/// is NOT reachable with real standard-channel messages because all
/// standard-channel messages encode to well under 1 KiB. We test the
/// `Broken`/`Decode` transition via this corrupted-payload path instead.
#[tokio::test]
async fn corrupted_payload_returns_decode_then_decode_again() {
    let mut session = Session::new().await;

    let message = SetupConnectionSuccess {
        used_version: 2,
        flags: 0,
    };
    let mut wire_buffer = [0u8; POOL_WIRE_BUFFER_SIZE];
    let wire_len = session
        .pool_codec
        .encode(&message, &mut wire_buffer)
        .expect("BUG: pool encode failed");
    let mut frame_bytes: Vec<u8> = wire_buffer[..wire_len].to_vec();

    // Flip a byte in the encrypted payload. The header occupies the first
    // `HEADER_WIRE_SIZE` bytes; we corrupt a byte in the payload body.
    let corrupt_offset = EncryptedCodec::HEADER_WIRE_SIZE + 1;
    if frame_bytes.len() > corrupt_offset {
        frame_bytes[corrupt_offset] ^= 0xFF;
    }

    let mut slow = SlowTransport::new(frame_bytes);

    let first_error = session
        .client
        .drive(&mut slow)
        .await
        .expect_err("BUG: drive with corrupted payload must fail");
    assert_eq!(
        first_error,
        Error::Decode,
        "first drive with corrupted payload must return Decode",
    );

    // Second drive: frame state is Broken; must return Decode without
    // touching the transport.
    let second_error = session
        .client
        .drive(&mut slow)
        .await
        .expect_err("BUG: second drive after corrupted frame must fail");
    assert_eq!(
        second_error,
        Error::Decode,
        "second drive after poisoned state must return Decode",
    );
}

// ── Extension frames ──────────────────────────────────────────────────────────────

/// An extension the mining message group does not know, as a test would
/// choose one.
const FOREIGN_EXTENSION: u16 = 0x1234;
const FOREIGN_MSG_TYPE: u8 = 7;
const FOREIGN_PAYLOAD: &[u8] = b"hello";

/// A frame of the foreign extension over `payload`, as the pool sends it.
fn foreign_frame(payload: &[u8]) -> FrameView<'_> {
    let descriptor = MessageDescriptor::new(FOREIGN_EXTENSION, FOREIGN_MSG_TYPE, false);
    let length = u32::try_from(payload.len()).expect("BUG: test payload fits a frame");
    FrameView::try_new(Header::for_descriptor(descriptor, length), payload)
        .expect("BUG: test frame is well formed")
}

/// A frame of another extension is handed to the visitor whole instead
/// of being dropped as an unknown mining message.
#[tokio::test]
async fn a_foreign_extension_frame_reaches_the_visitor() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    session.pool_send_msg(&foreign_frame(FOREIGN_PAYLOAD)).await;
    session.drive_and_accept(&mut visitor).await;

    assert_eq!(
        visitor.extension_frames,
        vec![(
            FOREIGN_EXTENSION,
            FOREIGN_MSG_TYPE,
            false,
            FOREIGN_PAYLOAD.to_vec()
        )],
    );
    assert!(visitor.jobs.is_empty() && visitor.session_updates.is_empty());
}

/// `send_extension` seals the frame into the outbound slot and the next
/// `drive` writes it, before it reads the pool's frame.
#[tokio::test]
async fn send_extension_seals_the_frame_for_the_next_drive() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();

    let frame = ExtensionFrame {
        is_channel_message: false,
        extension_type: FOREIGN_EXTENSION,
        msg_type: FOREIGN_MSG_TYPE,
        payload: FOREIGN_PAYLOAD,
    };
    session
        .client
        .send_extension(&frame)
        .expect("BUG: send_extension must succeed on an idle outbound slot");

    // An inbound frame so drive has something to read after the flush.
    session.pool_send_msg(&foreign_frame(b"pong")).await;
    session.drive_and_accept(&mut visitor).await;

    let (header, payload) = pool_recv_raw(&mut session.pool_codec, &mut session.pool_end).await;
    assert_eq!(header.extension_type, FOREIGN_EXTENSION);
    assert_eq!(header.msg_type, FOREIGN_MSG_TYPE);
    assert!(!header.is_channel_message);
    assert_eq!(payload, FOREIGN_PAYLOAD);
}

/// The outbound slot holds one sealed frame; a second send before a
/// drive is refused so the caller can retry after the flush.
#[tokio::test]
async fn send_extension_while_a_frame_waits_is_buffer_too_small() {
    let mut session = Session::new().await;
    let frame = ExtensionFrame {
        is_channel_message: false,
        extension_type: FOREIGN_EXTENSION,
        msg_type: FOREIGN_MSG_TYPE,
        payload: FOREIGN_PAYLOAD,
    };
    session
        .client
        .send_extension(&frame)
        .expect("BUG: the first send must succeed");
    assert_eq!(
        session.client.send_extension(&frame),
        Err(Error::BufferTooSmall),
    );
}

#[tokio::test]
async fn send_extension_before_connect_is_an_adapter_error() {
    let config = default_config();
    let mut client = StratumV2Client::new(&config, crypto_ctx(), OsRng);
    let frame = ExtensionFrame {
        is_channel_message: false,
        extension_type: FOREIGN_EXTENSION,
        msg_type: FOREIGN_MSG_TYPE,
        payload: FOREIGN_PAYLOAD,
    };
    assert_eq!(client.send_extension(&frame), Err(Error::Adapter));
}

/// A payload past `MAX_EXTENSION_PAYLOAD_BYTES` can never be sealed into
/// the outbound slot, so it is refused as a caller fault rather than as
/// a slot that will free up.
#[tokio::test]
async fn an_oversized_extension_payload_is_an_adapter_error() {
    let mut session = Session::new().await;
    let oversized = vec![0xAB; MAX_EXTENSION_PAYLOAD_BYTES + 1];
    let frame = ExtensionFrame {
        is_channel_message: false,
        extension_type: FOREIGN_EXTENSION,
        msg_type: FOREIGN_MSG_TYPE,
        payload: &oversized,
    };
    assert_eq!(session.client.send_extension(&frame), Err(Error::Adapter));

    let largest = vec![0xAB; MAX_EXTENSION_PAYLOAD_BYTES];
    let frame = ExtensionFrame {
        is_channel_message: false,
        payload: &largest,
        ..frame
    };
    assert_eq!(session.client.send_extension(&frame), Ok(()));
}

/// The descriptor keeps the channel flag through both adapter boundaries.
#[tokio::test]
async fn extension_channel_flags_round_trip() {
    let mut session = Session::new().await;
    let mut visitor = RecordingVisitor::default();
    let payload = FAKE_CHANNEL_ID.to_le_bytes();
    for is_channel_message in [false, true] {
        let header = Header::for_descriptor(
            MessageDescriptor::new(FOREIGN_EXTENSION, FOREIGN_MSG_TYPE, is_channel_message),
            u32::try_from(payload.len()).expect("payload fits"),
        );
        let incoming = FrameView::try_new(header, &payload).expect("valid frame");
        session.pool_send_msg(&incoming).await;
        session.drive_and_accept(&mut visitor).await;
        let received = visitor.extension_frames.last().expect("frame received");
        assert_eq!(received.2, is_channel_message);
        assert_eq!(received.3, payload);

        session
            .client
            .send_extension(&ExtensionFrame {
                extension_type: received.0,
                msg_type: received.1,
                is_channel_message: received.2,
                payload: &received.3,
            })
            .expect("slot free");
        session.pool_send_msg(&foreign_frame(b"pong")).await;
        session.drive_and_accept(&mut visitor).await;
        let (outgoing, body) = pool_recv_raw(&mut session.pool_codec, &mut session.pool_end).await;
        assert_eq!(outgoing, header);
        assert_eq!(body, payload);
    }
}
