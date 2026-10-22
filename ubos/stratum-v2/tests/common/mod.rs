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

//! Shared test support for the `stratum-v2` integration test crates.
//!
//! Cargo compiles this module into each integration-test binary that
//! declares `mod common;`, so each binary links its own copy and uses only
//! a subset of the items here. `#![allow(dead_code)]` silences the
//! per-binary unused warnings that result.

#![allow(dead_code)]

use std::string::String;
use std::sync::OnceLock;
use std::vec::Vec;

use ii_sv2::framing::FrameCodec;
use ii_sv2::security::{
    CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey, CryptoCtx,
    EncryptedCodec, PublicKey, Rng as Sv2Rng, Secp256k1KeyPair, SecretKey, secp256k1,
};
use mining_protocol::{
    ExtensionFrame, JobId, MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult,
};
use tokio::io::AsyncWriteExt as _;

// ── Crypto context ──────────────────────────────────────────────────────────────

/// Shared secp256k1 context initialised once per test binary.
static CRYPTO_CTX: OnceLock<CryptoCtx<secp256k1::All>> = OnceLock::new();

/// Lazily initialised secp256k1 context shared across a test binary.
pub fn crypto_ctx() -> &'static CryptoCtx<secp256k1::All> {
    CRYPTO_CTX.get_or_init(CryptoCtx::new)
}

// ── Self-signed certificate ──────────────────────────────────────────────────────

/// Build a self-signed `CertificateWithPrivateKey` for a fake pool. The
/// server and authority keypairs are drawn from the supplied RNGs; callers
/// pass an OS RNG for independence or a seeded RNG for reproducibility.
pub fn make_cert<R: Sv2Rng>(
    server_rng: &mut R,
    authority_rng: &mut R,
) -> CertificateWithPrivateKey {
    let ctx = crypto_ctx();
    let server_keypair = Secp256k1KeyPair::generate(server_rng, ctx);
    let authority_keypair = Secp256k1KeyPair::generate(authority_rng, ctx);
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

// ── Fake-pool wire helpers ───────────────────────────────────────────────────────

/// Wire buffer for a single encoded frame. Every standard-channel
/// pool→client message encodes to well under 512 bytes.
pub const POOL_WIRE_BUFFER_SIZE: usize = 512;

/// Encode `message` with `codec` and write the bytes to `writer`.
pub async fn pool_send<F>(
    codec: &mut EncryptedCodec,
    message: &F,
    writer: &mut tokio::io::DuplexStream,
) where
    F: ii_sv2::framing::IntoFrame,
{
    let mut wire = [0u8; POOL_WIRE_BUFFER_SIZE];
    let wire_len = codec
        .encode(message, &mut wire)
        .expect("BUG: pool_send encode failed");
    writer
        .write_all(&wire[..wire_len])
        .await
        .expect("BUG: pool_send write_all failed");
}

// ── Fake pool constants ──────────────────────────────────────────────────────────

/// Channel ID sent by the fake pool in every `OpenStandardMiningChannelSuccess`.
pub const FAKE_CHANNEL_ID: u32 = 42;

/// Wire job id sent by the fake pool in `NewMiningJob` / `SetNewPrevHash`.
pub const FAKE_JOB_ID: u32 = 7;

/// `nbits` used in all fake `SetNewPrevHash` frames.
pub const FAKE_NBITS: u32 = 0x1700_e9bd;

/// `min_ntime` used in `SetNewPrevHash` frames.
pub const FAKE_NTIME: u32 = 1_700_000_000;

/// A 32-byte target pattern used in `OpenStandardMiningChannelSuccess`.
pub const FAKE_TARGET: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// Extranonce prefix sent in `OpenStandardMiningChannelSuccess`.
pub const FAKE_EXTRANONCE_PREFIX: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];

/// A 32-byte prev-hash pattern used in `SetNewPrevHash`.
pub const FAKE_PREV_HASH: [u8; 32] = [0x01; 32];

/// A 32-byte merkle root used in `NewMiningJob`.
pub const FAKE_MERKLE_ROOT: [u8; 32] = [0xDE; 32];

// ── Recorded visitor callbacks ────────────────────────────────────────────────────

/// Recorded `SessionUpdate` variant, used for assertions in tests.
/// Variant names do not carry a common prefix to satisfy `clippy::enum_variant_names`.
#[derive(Debug, Clone)]
pub enum RecordedUpdate {
    /// Pool-provided share target as a little-endian 256-bit value.
    Target([u8; 32]),
    /// Pool-provided extranonce assignment.
    Extranonce { prefix: Vec<u8>, en2_size: u8 },
    /// Pool-provided version-rolling mask.
    VersionMask(u32),
}

/// Recorded `SubmitResult` for assertion in tests.
#[derive(Debug, Clone)]
pub enum RecordedSubmitResult {
    Accepted { count: u32, work: u64 },
    Rejected { error_message: Option<String> },
}

/// Recorded `MiningJob` as an owned copy of the borrowed fields.
#[derive(Debug, Clone)]
pub struct RecordedJob {
    pub id: JobId,
    pub clean_jobs: bool,
}

/// `MiningClientVisitor` that appends every callback into owned `Vec`s.
/// Tests inspect the vecs after `accept` returns.
#[derive(Default)]
pub struct RecordingVisitor {
    pub session_updates: Vec<RecordedUpdate>,
    pub submit_results: Vec<RecordedSubmitResult>,
    pub jobs: Vec<RecordedJob>,
    /// Every foreign extension frame as `(extension_type, msg_type, payload)`.
    pub extension_frames: Vec<(u16, u8, bool, Vec<u8>)>,
}

impl MiningClientVisitor for RecordingVisitor {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        self.jobs.push(RecordedJob {
            id: job.id,
            clean_jobs: job.clean_jobs,
        });
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        let recorded = match result {
            SubmitResult::Accepted { count, work } => {
                RecordedSubmitResult::Accepted { count, work }
            }
            SubmitResult::Rejected { error_message, .. } => RecordedSubmitResult::Rejected {
                error_message: error_message.map(str::to_owned),
            },
        };
        self.submit_results.push(recorded);
    }

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

    fn visit_extension_frame(&mut self, frame: ExtensionFrame<'_>) {
        self.extension_frames.push((
            frame.extension_type,
            frame.msg_type,
            frame.is_channel_message,
            frame.payload.to_vec(),
        ));
    }
}
