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
    accept, new_keypair, secp256k1, CertificateHeader, CertificateSigningRequest,
    CertificateWithPrivateKey, CryptoCtx, EncryptedCodec, EstablishedSession, PublicKey,
    Rng as Sv2Rng, SecretKey,
};
use ii_sv2::types::{Bytes0_32, Str0_255, Uint256};
use mining_protocol::{
    Error, MiningClient, MiningClientVisitor, MiningJob, SessionRejection, SessionUpdate,
    SubmitResult,
};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::sync::OnceLock;
use stratum_v2::{StratumV2Client, Sv2Config};
use tokio::io::AsyncReadExt as _;

// ── Crypto context ──────────────────────────────────────────────────────────────

/// Shared secp256k1 context initialised once per test binary.
static CRYPTO_CTX: OnceLock<CryptoCtx<secp256k1::All>> = OnceLock::new();

fn crypto_ctx() -> &'static CryptoCtx<secp256k1::All> {
    CRYPTO_CTX.get_or_init(CryptoCtx::new)
}

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
    let ctx = crypto_ctx();
    let server_keypair = new_keypair(&mut SeededRng::new(CERT_SERVER_SEED), ctx);
    let authority_keypair = new_keypair(&mut SeededRng::new(CERT_AUTHORITY_SEED), ctx);
    let header = CertificateHeader {
        version: 0,
        valid_from: 0,
        not_valid_after: u32::MAX,
        remote_static_public_key: PublicKey::from(&server_keypair),
    };
    let signed = CertificateSigningRequest { header }.sign(&authority_keypair, ctx.as_secp256k1());
    CertificateWithPrivateKey {
        header: signed.header,
        authority_public_key: signed.authority_public_key,
        server_static_private_key: SecretKey::from(&server_keypair),
        signature: signed.signature,
    }
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

// ── Fake-pool wire helpers ──────────────────────────────────────────────────────

/// Wire buffer for a single encoded pool→client frame.
const POOL_WIRE_BUFFER_SIZE: usize = 512;

/// Encode `message` with `codec` and write the bytes to `writer`.
async fn pool_send<F>(codec: &mut EncryptedCodec, message: &F, writer: &mut tokio::io::DuplexStream)
where
    F: ii_sv2::framing::IntoFrame,
{
    use tokio::io::AsyncWriteExt as _;
    let mut wire = [0u8; POOL_WIRE_BUFFER_SIZE];
    let wire_len = codec
        .encode(message, &mut wire)
        // BUG: pool_send encode must succeed with a correctly sized message
        .expect("BUG: pool_send encode failed");
    writer
        .write_all(&wire[..wire_len])
        .await
        // BUG: pool_send write_all must succeed over an in-memory duplex
        .expect("BUG: pool_send write_all failed");
}

// ── Fake pool constants (mirrored from client.rs) ───────────────────────────────

/// Channel ID sent by the fake pool in `OpenStandardMiningChannelSuccess`.
const FAKE_CHANNEL_ID: u32 = 42;

/// Wire job id sent by the fake pool in `NewMiningJob` / `SetNewPrevHash`.
const FAKE_JOB_ID: u32 = 7;

/// `nbits` used in fake `SetNewPrevHash` frames.
const FAKE_NBITS: u32 = 0x1700_e9bd;

/// `min_ntime` used in `SetNewPrevHash` frames.
const FAKE_NTIME: u32 = 1_700_000_000;

/// 32-byte target pattern used in `OpenStandardMiningChannelSuccess`.
const FAKE_TARGET: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// Extranonce prefix in `OpenStandardMiningChannelSuccess`.
const FAKE_EXTRANONCE_PREFIX: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];

/// 32-byte prev-hash pattern used in `SetNewPrevHash`.
const FAKE_PREV_HASH: [u8; 32] = [0x01; 32];

/// 32-byte merkle root used in `NewMiningJob`.
const FAKE_MERKLE_ROOT: [u8; 32] = [0xDE; 32];

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

// ── RecordingVisitor (local copy, not shared with client.rs) ───────────────────

/// Recorded `SessionUpdate` variant for golden-test assertions.
/// Variant names do not carry a common prefix to satisfy `clippy::enum_variant_names`.
#[derive(Debug, Clone)]
enum RecordedUpdate {
    /// Pool-provided share target as a little-endian 256-bit value.
    Target([u8; 32]),
    /// Pool-provided extranonce assignment.
    Extranonce { prefix: Vec<u8>, en2_size: u8 },
    /// Pool-provided version-rolling mask.
    VersionMask(u32),
}

/// Recorded `MiningJob` as an owned copy of the borrowed fields.
#[derive(Debug, Clone)]
struct RecordedJob {
    clean_jobs: bool,
}

/// `MiningClientVisitor` that appends every callback into owned `Vec`s.
/// Tests inspect the vecs after `accept` returns.
#[derive(Default)]
struct RecordingVisitor {
    session_updates: Vec<RecordedUpdate>,
    jobs: Vec<RecordedJob>,
}

impl MiningClientVisitor for RecordingVisitor {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        self.jobs.push(RecordedJob {
            clean_jobs: job.clean_jobs,
        });
    }

    fn visit_submit_result(&mut self, _result: SubmitResult<'_>) {}

    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        let recorded = match update {
            SessionUpdate::SetTarget(target) => RecordedUpdate::Target(*target),
            SessionUpdate::SetExtranonce { prefix, en2_size } => RecordedUpdate::Extranonce {
                prefix: prefix.to_vec(),
                en2_size,
            },
            SessionUpdate::SetVersionMask(mask) => RecordedUpdate::VersionMask(mask),
            SessionUpdate::Reconnect { .. } => return,
        };
        self.session_updates.push(recorded);
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
        // BUG: client-frame header read must succeed over the in-memory duplex
        .expect("BUG: drain header read failed");
    let header = codec
        .decode_header(&mut header_buf)
        // BUG: decode_header must succeed for a valid noise frame
        .expect("BUG: drain decode_header failed");
    let payload_wire = EncryptedCodec::payload_wire_size(header.msg_length as usize);
    let mut payload = vec![0u8; payload_wire];
    pool_duplex
        .read_exact(&mut payload)
        .await
        // BUG: client-frame payload read must succeed over the in-memory duplex
        .expect("BUG: drain payload read failed");
    codec
        .decode_frame(header, &mut payload)
        // BUG: decode_frame must succeed for a valid noise frame
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
            let mut pool_duplex = &mut pool_stream;
            let session: EstablishedSession = {
                let mut wrapped = FromTokio::new(&mut pool_duplex);
                accept(&mut wrapped, SeededRng::new(POOL_SEED), crypto_ctx(), &cert)
                    .await
                    // BUG: fake-pool accept must succeed with the seeded cert and RNG
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
                    // BUG: FAKE_EXTRANONCE_PREFIX fits in Bytes0_32
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
    // BUG: connect must succeed against the seeded pool responder
    connect_ok.expect("BUG: connect failed in success generator");

    // Drive the client through the 4 pool→client frames.
    let mut visitor = RecordingVisitor::default();
    let mut recording2 = RecordingTransport::new(&mut client_stream);

    for _ in 0..4_usize {
        client
            .drive(&mut recording2)
            .await
            // BUG: drive must succeed for the scripted pool messages
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
            let mut pool_duplex = &mut pool_stream;
            let session: EstablishedSession = {
                let mut wrapped = FromTokio::new(&mut pool_duplex);
                accept(&mut wrapped, SeededRng::new(POOL_SEED), crypto_ctx(), &cert)
                    .await
                    // BUG: fake-pool accept must succeed
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
                    // BUG: "invalid-flags" is 13 bytes, within the 255-byte cap
                    .expect("BUG: static string fits in Str0_255"),
            };
            pool_send(&mut codec, &error_message, pool_duplex).await;
        }
    );

    let (connect_ok, client_to_pool, pool_to_client) = connect_result;
    // BUG: connect must succeed against the seeded pool responder
    connect_ok.expect("BUG: connect failed in reject generator");

    // Drive the client through SetupConnectionError.
    let mut visitor = RecordingVisitor::default();
    let mut recording2 = RecordingTransport::new(&mut client_stream);

    client
        .drive(&mut recording2)
        .await
        // BUG: first drive must succeed (decodes SetupConnectionError)
        .expect("BUG: first drive failed in reject generator");
    client.accept(&mut visitor);

    let mut full_pool_to_client = pool_to_client;
    full_pool_to_client.extend_from_slice(&recording2.pool_to_client);
    let mut full_client_to_pool = client_to_pool;
    full_client_to_pool.extend_from_slice(&recording2.client_to_pool);

    (full_pool_to_client, full_client_to_pool)
}

// ── Fixture generator (ignored, run explicitly to regenerate) ──────────────────

/// Generate the four golden fixture files under `tests/fixtures/`. Run with:
///
/// ```text
/// cargo test -p stratum-v2 --test golden_transcript -- --ignored generate_golden_fixtures
/// ```
#[tokio::test]
#[ignore = "regenerates committed fixtures; run explicitly, not in the default gate"]
async fn generate_golden_fixtures() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        // BUG: CARGO_MANIFEST_DIR is always set by cargo test
        .expect("BUG: CARGO_MANIFEST_DIR not set");
    let fixtures_dir = std::path::Path::new(&manifest_dir).join("tests/fixtures");
    std::fs::create_dir_all(&fixtures_dir)
        // BUG: fixtures directory creation must succeed in the crate tree
        .expect("BUG: failed to create tests/fixtures directory");

    let (success_pool_to_client, success_client_to_pool) = run_success_exchange().await;
    let (reject_pool_to_client, reject_client_to_pool) = run_reject_exchange().await;

    std::fs::write(
        fixtures_dir.join("golden_success_pool_to_client.bin"),
        &success_pool_to_client,
    )
    // BUG: fixture file write must succeed
    .expect("BUG: failed to write golden_success_pool_to_client.bin");
    std::fs::write(
        fixtures_dir.join("golden_success_client_to_pool.bin"),
        &success_client_to_pool,
    )
    // BUG: fixture file write must succeed
    .expect("BUG: failed to write golden_success_client_to_pool.bin");
    std::fs::write(
        fixtures_dir.join("golden_reject_pool_to_client.bin"),
        &reject_pool_to_client,
    )
    // BUG: fixture file write must succeed
    .expect("BUG: failed to write golden_reject_pool_to_client.bin");
    std::fs::write(
        fixtures_dir.join("golden_reject_client_to_pool.bin"),
        &reject_client_to_pool,
    )
    // BUG: fixture file write must succeed
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

/// Replay the committed success transcript.
///
/// Feeds `GOLDEN_SUCCESS_POOL_TO_CLIENT` through a `ReplayTransport` and
/// drives the client with the same seeded RNG used by the generator. Asserts:
///
/// - The bytes the client wrote equal `GOLDEN_SUCCESS_CLIENT_TO_POOL` exactly
///   (wire anchor).
/// - The visitor recorded `SetTarget` and `SetExtranonce` from the channel-open
///   and a clean job from the future-job + `SetNewPrevHash` pair.
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
        // BUG: connect must succeed replaying the committed handshake bytes
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

    // Index 0: SetVersionMask(0) from SetupConnectionSuccess(flags=0).
    assert!(
        matches!(visitor.session_updates[0], RecordedUpdate::VersionMask(0)),
        "first session update must be SetVersionMask(0), got {:?}",
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

/// Replay the committed reject transcript.
///
/// Feeds `GOLDEN_REJECT_POOL_TO_CLIENT` through a `ReplayTransport` and
/// asserts that after consuming `SetupConnectionError` the next `drive` returns
/// `Err(Error::SessionRejected(SessionRejection::SetupConnectionError))`.
#[tokio::test]
async fn golden_reject_transcript_replays() {
    let config = golden_config();
    let config: &'static Sv2Config = Box::leak(Box::new(config));
    let mut client = StratumV2Client::new(config, crypto_ctx(), SeededRng::new(CLIENT_SEED));

    let mut replay = ReplayTransport::new(GOLDEN_REJECT_POOL_TO_CLIENT);

    client
        .connect(&mut replay)
        .await
        // BUG: connect must succeed replaying the committed reject handshake bytes
        .expect("BUG: connect failed replaying reject fixture");

    let mut visitor = RecordingVisitor::default();

    // Consume the SetupConnectionError frame.
    client
        .drive(&mut replay)
        .await
        // BUG: first drive must succeed (decodes SetupConnectionError)
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
        // BUG: second drive must fail after SetupConnectionError was accepted
        .expect_err("BUG: second drive should fail after setup-connection-error");
    assert_eq!(
        error,
        Error::SessionRejected(SessionRejection::SetupConnectionError),
        "drive after setup-connection-error must return SessionRejected(SetupConnectionError)",
    );
}
