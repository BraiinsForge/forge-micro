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

//! Known-answer / golden-transcript replay tests in the RFC-8448 style.
//!
//! Fixture files under `tests/fixtures/` are committed byte recordings of the
//! complete wire exchange for two scenarios (success and reject). Because
//! every source of randomness flows through [`SeededRng`] with a fixed seed,
//! the on-wire byte stream is fully reproducible across runs and platforms.
//! These tests assert that the client emits and accepts exactly those bytes,
//! anchoring the wire format against unintended changes.
//!
//! Fixtures are regenerated with:
//!
//! ```text
//! cargo test -p stratum-v2 --test golden_transcript -- --ignored generate_golden_fixtures
//! ```

extern crate std;

use std::vec::Vec;

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{ErrorType, Read, Write};
use ii_sv2::framing::FrameCodec;
use ii_sv2::messages::{
    NewMiningJob, OpenStandardMiningChannelSuccess, SetNewPrevHash, SetupConnectionError,
    SetupConnectionSuccess,
};
use ii_sv2::security::{
    CertificateWithPrivateKey, EncryptedCodec, EstablishedSession, Rng as Sv2Rng, accept,
};
use ii_sv2::types::{Bytes0_32, Str0_255, Uint256};
use mining_protocol::{Error, MiningClient, SessionRejection};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use stratum_v2::{StratumV2Client, Sv2Config};
use tokio::io::AsyncReadExt as _;

mod common;

use common::{
    FAKE_CHANNEL_ID, FAKE_EXTRANONCE_PREFIX, FAKE_JOB_ID, FAKE_MERKLE_ROOT, FAKE_NBITS, FAKE_NTIME,
    FAKE_PREV_HASH, FAKE_TARGET, RecordedUpdate, RecordingVisitor, crypto_ctx, make_cert,
    pool_send,
};

// ── Seeded RNG ──────────────────────────────────────────────────────────────────

/// `Sv2Rng` backed by a seeded `ChaCha20Rng`. Using a fixed seed makes
/// keypair generation and handshake randomness fully deterministic, so the
/// complete on-wire byte stream is reproducible.
struct SeededRng(ChaCha20Rng);

impl SeededRng {
    /// Construct a `SeededRng` from a fixed `u64` seed.
    fn new(seed: u64) -> Self {
        Self(ChaCha20Rng::seed_from_u64(seed))
    }
}

impl Sv2Rng for SeededRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }
}

// ── Deterministic cert builder ──────────────────────────────────────────────────

/// Certificate seeds: distinct values so each stream is independent.
const CERT_SERVER_SEED: u64 = 0xDEAD_BEEF_0001;
const CERT_AUTHORITY_SEED: u64 = 0xDEAD_BEEF_0002;

/// Build a self-signed `CertificateWithPrivateKey` whose keypairs are derived
/// from fixed seeds. Identical to `make_server_cert` in `client.rs` except
/// that `SeededRng` replaces `OsRng`, making the output deterministic.
fn make_seeded_server_cert() -> CertificateWithPrivateKey {
    make_cert(
        &mut SeededRng::new(CERT_SERVER_SEED),
        &mut SeededRng::new(CERT_AUTHORITY_SEED),
    )
}

// ── Fixture paths and embedded fixtures ────────────────────────────────────────

/// Committed pool→client byte recording for the success scenario.
static GOLDEN_SUCCESS_POOL_TO_CLIENT: &[u8] =
    include_bytes!("fixtures/golden_success_pool_to_client.bin");

/// Committed client→pool byte recording for the success scenario.
static GOLDEN_SUCCESS_CLIENT_TO_POOL: &[u8] =
    include_bytes!("fixtures/golden_success_client_to_pool.bin");

/// Committed pool→client byte recording for the reject scenario.
static GOLDEN_REJECT_POOL_TO_CLIENT: &[u8] =
    include_bytes!("fixtures/golden_reject_pool_to_client.bin");

/// Committed client→pool byte recording for the reject scenario.
static GOLDEN_REJECT_CLIENT_TO_POOL: &[u8] =
    include_bytes!("fixtures/golden_reject_client_to_pool.bin");

// ── Seeds for deterministic exchange ───────────────────────────────────────────

/// Client RNG seed: used for the noise initiator's ephemeral key.
const CLIENT_SEED: u64 = 0xCAFE_BABE_0001;

/// Pool responder RNG seed: used for the noise responder's ephemeral key.
const POOL_SEED: u64 = 0xCAFE_BABE_0002;

// ── RecordingTransport ──────────────────────────────────────────────────────────

/// Transport that wraps a `FromTokio<DuplexStream>` and records every byte
/// the client reads (pool→client direction) and every byte the client writes
/// (client→pool direction). Used by the generator to capture raw wire bytes
/// in both directions simultaneously.
struct RecordingTransport<'a> {
    inner: FromTokio<&'a mut tokio::io::DuplexStream>,
    /// Bytes the client wrote toward the pool.
    client_to_pool: Vec<u8>,
    /// Bytes the client read from the pool.
    pool_to_client: Vec<u8>,
}

impl<'a> RecordingTransport<'a> {
    fn new(stream: &'a mut tokio::io::DuplexStream) -> Self {
        Self {
            inner: FromTokio::new(stream),
            client_to_pool: Vec::new(),
            pool_to_client: Vec::new(),
        }
    }
}

impl ErrorType for RecordingTransport<'_> {
    type Error = std::io::Error;
}

impl Read for RecordingTransport<'_> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let count = self.inner.read(buffer).await?;
        self.pool_to_client.extend_from_slice(&buffer[..count]);
        Ok(count)
    }
}

impl Write for RecordingTransport<'_> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let count = self.inner.write(buffer).await?;
        self.client_to_pool.extend_from_slice(&buffer[..count]);
        Ok(count)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

// ── ReplayTransport ─────────────────────────────────────────────────────────────

/// Transport that serves committed fixture bytes to the client on `read` and
/// records whatever the client writes for later byte-level assertion. `read`
/// returns `Ok(0)` when the fixture bytes are exhausted (EOF).
struct ReplayTransport {
    /// Fixture bytes to serve to the client (pool→client recording).
    readable: &'static [u8],
    /// Current read position in `readable`.
    read_position: usize,
    /// Bytes the client has written during the replay (client→pool recording).
    written: Vec<u8>,
}

impl ReplayTransport {
    /// Construct a `ReplayTransport` from a static fixture slice.
    fn new(readable: &'static [u8]) -> Self {
        Self {
            readable,
            read_position: 0,
            written: Vec::new(),
        }
    }
}

impl ErrorType for ReplayTransport {
    type Error = core::convert::Infallible;
}

impl Read for ReplayTransport {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let available = self.readable.len() - self.read_position;
        if available == 0 {
            return Ok(0);
        }
        let count = buffer.len().min(available);
        buffer[..count]
            .copy_from_slice(&self.readable[self.read_position..self.read_position + count]);
        self.read_position += count;
        Ok(count)
    }
}

impl Write for ReplayTransport {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.written.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// ── Default config ──────────────────────────────────────────────────────────────

/// Build the `Sv2Config` used in all golden tests.
fn golden_config() -> Sv2Config {
    Sv2Config::new("test.worker", None).expect("BUG: worker name fits in 128 bytes")
}

// ── Generator ──────────────────────────────────────────────────────────────────

/// Read and decode one full noise frame on the pool side, discarding the
/// payload. Used to drain the client→pool handshake frames
/// (`SetupConnection`, `OpenStandardMiningChannel`) before scripting replies.
async fn drain_client_frame(codec: &mut EncryptedCodec, pool_duplex: &mut tokio::io::DuplexStream) {
    let mut header_buf = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
    pool_duplex
        .read_exact(&mut header_buf)
        .await
        .expect("BUG: drain header read failed");
    let header = codec
        .decode_header(&mut header_buf)
        .expect("BUG: drain decode_header failed");
    let payload_wire = EncryptedCodec::payload_wire_size(header.msg_length as usize);
    let mut payload = vec![0u8; payload_wire];
    pool_duplex
        .read_exact(&mut payload)
        .await
        .expect("BUG: drain payload read failed");
    codec
        .decode_frame(header, &mut payload)
        .expect("BUG: drain decode_frame failed");
}

/// Run the full success exchange over a live duplex with a recording transport.
/// Returns `(pool_to_client_bytes, client_to_pool_bytes)`.
async fn run_success_exchange() -> (Vec<u8>, Vec<u8>) {
    let cert = make_seeded_server_cert();
    let (mut client_stream, mut pool_stream) = tokio::io::duplex(65536);
    let config = golden_config();
    let config: &'static Sv2Config = Box::leak(Box::new(config));
    let mut client = StratumV2Client::new(config, crypto_ctx(), SeededRng::new(CLIENT_SEED));

    let (connect_result, _pool_arm) = tokio::join!(
        async {
            let mut recording = RecordingTransport::new(&mut client_stream);
            let result = client.connect(&mut recording).await;
            (result, recording.client_to_pool, recording.pool_to_client)
        },
        async {
            let pool_duplex = &mut pool_stream;
            let session: EstablishedSession = {
                let mut wrapped = FromTokio::new(&mut *pool_duplex);
                accept(&mut wrapped, SeededRng::new(POOL_SEED), crypto_ctx(), &cert)
                    .await
                    .expect("BUG: fake-pool accept failed")
            };
            let (codec, _session_id) = EncryptedCodec::from_handshake(session);
            let mut codec = codec;

            // Drain the two client→pool handshake frames before scripting replies.
            for _ in 0..2_usize {
                drain_client_frame(&mut codec, pool_duplex).await;
            }

            // Now send the success script.
            let setup_success = SetupConnectionSuccess {
                used_version: 2,
                flags: 0,
            };
            pool_send(&mut codec, &setup_success, pool_duplex).await;

            let open_success = OpenStandardMiningChannelSuccess {
                request_id: 1,
                channel_id: FAKE_CHANNEL_ID,
                target: Uint256::from_le_bytes(&FAKE_TARGET),
                extranonce_prefix: Bytes0_32::try_from(FAKE_EXTRANONCE_PREFIX.as_slice())
                    .expect("BUG: FAKE_EXTRANONCE_PREFIX too long"),
                group_channel_id: 0,
            };
            pool_send(&mut codec, &open_success, pool_duplex).await;

            let future_job = NewMiningJob {
                channel_id: FAKE_CHANNEL_ID,
                job_id: FAKE_JOB_ID,
                min_ntime: None,
                version: 0x2000_0000,
                merkle_root: Uint256::from_le_bytes(&FAKE_MERKLE_ROOT),
            };
            pool_send(&mut codec, &future_job, pool_duplex).await;

            let prev_hash = SetNewPrevHash {
                channel_id: FAKE_CHANNEL_ID,
                job_id: FAKE_JOB_ID,
                prev_hash: Uint256::from_le_bytes(&FAKE_PREV_HASH),
                min_ntime: FAKE_NTIME,
                nbits: FAKE_NBITS,
            };
            pool_send(&mut codec, &prev_hash, pool_duplex).await;

            // The client reads these frames after the join, so the recording
            // transport on the client side captures the pool→client bytes.
        }
    );

    let (connect_ok, client_to_pool, pool_to_client) = connect_result;
    connect_ok.expect("BUG: connect failed in success generator");

    // Drive the client through the 4 pool→client frames.
    let mut visitor = RecordingVisitor::default();
    let mut recording2 = RecordingTransport::new(&mut client_stream);

    for _ in 0..4_usize {
        client
            .drive(&mut recording2)
            .await
            .expect("BUG: drive failed in success generator");
        client.accept(&mut visitor);
    }

    // pool_to_client captured by the connect-phase recording + the drive-phase recording.
    let mut full_pool_to_client = pool_to_client;
    full_pool_to_client.extend_from_slice(&recording2.pool_to_client);
    let mut full_client_to_pool = client_to_pool;
    full_client_to_pool.extend_from_slice(&recording2.client_to_pool);

    (full_pool_to_client, full_client_to_pool)
}

/// Run the full reject exchange over a live duplex with a recording transport.
/// Returns `(pool_to_client_bytes, client_to_pool_bytes)`.
async fn run_reject_exchange() -> (Vec<u8>, Vec<u8>) {
    let cert = make_seeded_server_cert();
    let (mut client_stream, mut pool_stream) = tokio::io::duplex(65536);
    let config = golden_config();
    let config: &'static Sv2Config = Box::leak(Box::new(config));
    let mut client = StratumV2Client::new(config, crypto_ctx(), SeededRng::new(CLIENT_SEED));

    let (connect_result, _pool_arm) = tokio::join!(
        async {
            let mut recording = RecordingTransport::new(&mut client_stream);
            let result = client.connect(&mut recording).await;
            (result, recording.client_to_pool, recording.pool_to_client)
        },
        async {
            let pool_duplex = &mut pool_stream;
            let session: EstablishedSession = {
                let mut wrapped = FromTokio::new(&mut *pool_duplex);
                accept(&mut wrapped, SeededRng::new(POOL_SEED), crypto_ctx(), &cert)
                    .await
                    .expect("BUG: fake-pool accept failed")
            };
            let (codec, _session_id) = EncryptedCodec::from_handshake(session);
            let mut codec = codec;

            // Drain the two client→pool handshake frames before scripting replies.
            for _ in 0..2_usize {
                drain_client_frame(&mut codec, pool_duplex).await;
            }

            // Send SetupConnectionError.
            let error_message = SetupConnectionError {
                flags: 0,
                code: Str0_255::try_from("invalid-flags")
                    .expect("BUG: static string fits in Str0_255"),
            };
            pool_send(&mut codec, &error_message, pool_duplex).await;
        }
    );

    let (connect_ok, client_to_pool, pool_to_client) = connect_result;
    connect_ok.expect("BUG: connect failed in reject generator");

    // Drive the client through SetupConnectionError.
    let mut visitor = RecordingVisitor::default();
    let mut recording2 = RecordingTransport::new(&mut client_stream);

    client
        .drive(&mut recording2)
        .await
        .expect("BUG: first drive failed in reject generator");
    client.accept(&mut visitor);

    let mut full_pool_to_client = pool_to_client;
    full_pool_to_client.extend_from_slice(&recording2.pool_to_client);
    let mut full_client_to_pool = client_to_pool;
    full_client_to_pool.extend_from_slice(&recording2.client_to_pool);

    (full_pool_to_client, full_client_to_pool)
}

// ── Fixture generator (ignored, run explicitly to regenerate) ──────────────────

// Generate the four golden fixture files under `tests/fixtures/`. Run with:
//
// ```text
// cargo test -p stratum-v2 --test golden_transcript -- --ignored generate_golden_fixtures
// ```
#[tokio::test]
#[ignore = "regenerates committed fixtures; run explicitly, not in the default gate"]
async fn generate_golden_fixtures() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("BUG: CARGO_MANIFEST_DIR not set");
    let fixtures_dir = std::path::Path::new(&manifest_dir).join("tests/fixtures");
    std::fs::create_dir_all(&fixtures_dir).expect("BUG: failed to create tests/fixtures directory");

    let (success_pool_to_client, success_client_to_pool) = run_success_exchange().await;
    let (reject_pool_to_client, reject_client_to_pool) = run_reject_exchange().await;

    std::fs::write(
        fixtures_dir.join("golden_success_pool_to_client.bin"),
        &success_pool_to_client,
    )
    .expect("BUG: failed to write golden_success_pool_to_client.bin");
    std::fs::write(
        fixtures_dir.join("golden_success_client_to_pool.bin"),
        &success_client_to_pool,
    )
    .expect("BUG: failed to write golden_success_client_to_pool.bin");
    std::fs::write(
        fixtures_dir.join("golden_reject_pool_to_client.bin"),
        &reject_pool_to_client,
    )
    .expect("BUG: failed to write golden_reject_pool_to_client.bin");
    std::fs::write(
        fixtures_dir.join("golden_reject_client_to_pool.bin"),
        &reject_client_to_pool,
    )
    .expect("BUG: failed to write golden_reject_client_to_pool.bin");

    std::println!("Generated fixtures in {}:", fixtures_dir.display());
    std::println!(
        "  golden_success_pool_to_client.bin  {} bytes",
        success_pool_to_client.len()
    );
    std::println!(
        "  golden_success_client_to_pool.bin  {} bytes",
        success_client_to_pool.len()
    );
    std::println!(
        "  golden_reject_pool_to_client.bin   {} bytes",
        reject_pool_to_client.len()
    );
    std::println!(
        "  golden_reject_client_to_pool.bin   {} bytes",
        reject_client_to_pool.len()
    );
}

// ── Replay tests ────────────────────────────────────────────────────────────────

// Replay the committed success transcript.
//
// Feeds `GOLDEN_SUCCESS_POOL_TO_CLIENT` through a `ReplayTransport` and
// drives the client with the same seeded RNG used by the generator. Asserts:
//
// - The bytes the client wrote equal `GOLDEN_SUCCESS_CLIENT_TO_POOL` exactly
//   (wire anchor).
// - The visitor recorded `SetTarget` and `SetExtranonce` from the channel-open
//   and a clean job from the future-job + `SetNewPrevHash` pair.
#[tokio::test]
async fn golden_success_transcript_replays() {
    let config = golden_config();
    let config: &'static Sv2Config = Box::leak(Box::new(config));
    let mut client = StratumV2Client::new(config, crypto_ctx(), SeededRng::new(CLIENT_SEED));

    let mut replay = ReplayTransport::new(GOLDEN_SUCCESS_POOL_TO_CLIENT);

    // connect() will drive the noise handshake using the seeded client RNG.
    // The fixture bytes on the read side contain the pool's handshake reply
    // (seeded with POOL_SEED during generation), so the AEAD chains align.
    client
        .connect(&mut replay)
        .await
        .expect("BUG: connect failed replaying success fixture");

    let mut visitor = RecordingVisitor::default();

    // Drive through 4 frames: SetupConnectionSuccess, OpenStandardMiningChannelSuccess,
    // NewMiningJob (future), SetNewPrevHash.
    for drive_index in 0..4_usize {
        client
            .drive(&mut replay)
            .await
            .unwrap_or_else(|error| panic!("BUG: drive {drive_index} failed: {error:?}"));
        client.accept(&mut visitor);
    }

    // Wire anchor: client output must match the committed fixture exactly.
    assert_eq!(
        replay.written, GOLDEN_SUCCESS_CLIENT_TO_POOL,
        "client→pool bytes must match the committed golden fixture",
    );

    // Visitor callback assertions.
    assert_eq!(
        visitor.session_updates.len(),
        3,
        "success replay must fire exactly VersionMask + SetTarget + SetExtranonce, got {}",
        visitor.session_updates.len(),
    );

    // Index 0: the full BIP320 mask from SetupConnectionSuccess(flags=0),
    // whose flags carry the upstream's requirements and pin nothing.
    assert!(
        matches!(
            visitor.session_updates[0],
            RecordedUpdate::VersionMask(0x1FFF_E000)
        ),
        "first session update must grant the full BIP320 mask, got {:?}",
        visitor.session_updates[0],
    );

    // Index 1: SetTarget from OpenStandardMiningChannelSuccess.
    match &visitor.session_updates[1] {
        RecordedUpdate::Target(target) => {
            assert_eq!(
                target, &FAKE_TARGET,
                "SetTarget must carry the pool's target bytes",
            );
        }
        other => panic!("expected Target update at index 1, got {other:?}"),
    }

    // Index 2: SetExtranonce from OpenStandardMiningChannelSuccess.
    match &visitor.session_updates[2] {
        RecordedUpdate::Extranonce { prefix, en2_size } => {
            assert_eq!(
                prefix.as_slice(),
                FAKE_EXTRANONCE_PREFIX.as_slice(),
                "SetExtranonce must carry the pool's prefix bytes",
            );
            assert_eq!(*en2_size, 0, "V2 standard channels have en2_size = 0");
        }
        other => panic!("expected Extranonce update at index 2, got {other:?}"),
    }

    // One clean job from the SetNewPrevHash activating the future job.
    assert_eq!(
        visitor.jobs.len(),
        1,
        "SetNewPrevHash must activate exactly one clean job",
    );
    assert!(
        visitor.jobs[0].clean_jobs,
        "job activated by SetNewPrevHash must have clean_jobs = true",
    );
}

// Replay the committed reject transcript.
//
// Feeds `GOLDEN_REJECT_POOL_TO_CLIENT` through a `ReplayTransport` and
// asserts that after consuming `SetupConnectionError` the next `drive` returns
// `Err(Error::SessionRejected(SessionRejection::SetupConnectionError))`.
#[tokio::test]
async fn golden_reject_transcript_replays() {
    let config = golden_config();
    let config: &'static Sv2Config = Box::leak(Box::new(config));
    let mut client = StratumV2Client::new(config, crypto_ctx(), SeededRng::new(CLIENT_SEED));

    let mut replay = ReplayTransport::new(GOLDEN_REJECT_POOL_TO_CLIENT);

    client
        .connect(&mut replay)
        .await
        .expect("BUG: connect failed replaying reject fixture");

    let mut visitor = RecordingVisitor::default();

    // Consume the SetupConnectionError frame.
    client
        .drive(&mut replay)
        .await
        .expect("BUG: first drive failed replaying reject fixture");
    client.accept(&mut visitor);

    // Wire anchor: bytes the client wrote must match the committed fixture.
    assert_eq!(
        replay.written, GOLDEN_REJECT_CLIENT_TO_POOL,
        "client→pool bytes must match the committed golden reject fixture",
    );

    // Next drive must return SessionRejected because accept transitioned
    // the client to Rejected state.
    let error = client
        .drive(&mut replay)
        .await
        .expect_err("BUG: second drive should fail after setup-connection-error");
    assert_eq!(
        error,
        Error::SessionRejected(SessionRejection::SetupConnectionError),
        "drive after setup-connection-error must return SessionRejected(SetupConnectionError)",
    );
}
