//! End-to-end smoke test of the V2 adapter against a real pool.
//!
//! Constructs a `StratumV2Client`, drives `connect` (noise NX
//! handshake → `SetupConnection` → `OpenStandardMiningChannel`),
//! and then loops `drive` / `accept` over a tokio `TcpStream` wrapped
//! via `embedded-io-adapters`, printing every visitor callback.
//! `submit` is not implemented yet (V2 phase 13), so this probe is
//! read-only.
//!
//! Run with:
//!
//! ```sh
//! POOL=pool.example:34254 \
//! USER=worker.name \
//! POOL_PUBKEY=<base58check pool authority public key> \
//!   cargo run --example client_probe -p stratum-v2
//! ```
//!
//! `POOL_PUBKEY` is the SV2-spec base58check encoding (the same
//! string the pool publishes). It is optional — if unset, the
//! probe falls back to `ValidateNothing`, which accepts any
//! responder certificate (suitable only for test pools you
//! control).

use std::env;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use embedded_io_adapters::tokio_1::FromTokio;
use ii_sv2::security::{secp256k1, CryptoCtx, PublicKey, Rng as Sv2Rng};
use mining_protocol::{
    JobId, MiningClient, MiningClientVisitor, MiningJob, SessionUpdate, Share, SubmitResult,
};
use rand::rngs::OsRng;
use rand::RngCore;
use stratum_v2::{StratumV2Client, Sv2Config};
use tokio::net::TcpStream;

/// `secp256k1::All` is alloc-backed; on host it is the simplest
/// flavor. `OnceLock` ensures the precomputation tables are built
/// just once for the run.
static CRYPTO_CTX: OnceLock<CryptoCtx<secp256k1::All>> = OnceLock::new();

fn crypto_ctx() -> &'static CryptoCtx<secp256k1::All> {
    CRYPTO_CTX.get_or_init(CryptoCtx::new)
}

/// `Rng` adapter for `ii_sv2::security::Rng`. Backed by the OS
/// CSPRNG via `rand`.
struct ProbeRng;

impl Sv2Rng for ProbeRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        OsRng.fill_bytes(dest);
    }
}

#[derive(Default)]
struct PrintVisitor {
    /// Most recent `JobId` seen, so the probe can submit a share
    /// against a known-valid job. `None` until the first
    /// `visit_new_job`.
    latest_job: Option<JobId>,
}

impl MiningClientVisitor for PrintVisitor {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        println!(
            "[job] id={:?} version={:#010x} mask={:#010x} nbits={:#010x} ntime={:#010x} \
             clean_jobs={} en2_size={} prev_hash={} merkle_marker={}",
            job.id,
            job.version,
            job.version_rolling_mask,
            job.nbits,
            job.ntime,
            job.clean_jobs,
            job.extranonce_2_size,
            hex_msb(job.prev_hash),
            merkle_marker(&job),
        );
        self.latest_job = Some(job.id);
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        match result {
            SubmitResult::Accepted { count, work } => {
                println!("[submit-result] accepted count={count} work={work}");
            }
            SubmitResult::Rejected {
                error_code,
                error_message,
            } => {
                println!(
                    "[submit-result] rejected code={error_code:?} message={error_message:?}",
                );
            }
        }
    }

    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        match update {
            SessionUpdate::SetTarget(target) => {
                println!("[set-target] {}", hex_msb(target));
            }
            SessionUpdate::SetExtranonce { prefix, en2_size } => {
                println!(
                    "[set-extranonce] prefix={} en2_size={}",
                    hex_lsb(prefix),
                    en2_size,
                );
            }
            SessionUpdate::SetVersionMask(mask) => {
                println!("[set-version-mask] {mask:#010x}");
            }
            SessionUpdate::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                println!("[reconnect] host={host} port={port} wait={wait_seconds}");
            }
        }
    }
}

/// Compact way to confirm V2 hands us a final root rather than V1's
/// prepared materials. `LazyMerkle::Final` is the V2 path.
fn merkle_marker(job: &MiningJob<'_>) -> &'static str {
    match job.merkle {
        mining_protocol::LazyMerkle::Final(_) => "Final",
        mining_protocol::LazyMerkle::Prepared { .. } => "Prepared",
    }
}

fn hex_lsb(bytes: &[u8]) -> String {
    let mut out = vec![0u8; bytes.len() * 2];
    let ok = mining_protocol::hex::encode_lower_to_slice(bytes, &mut out);
    assert!(ok, "hex_lsb buffer sized for input");
    String::from_utf8(out).expect("hex encoder emits ASCII")
}

/// Print a 256-bit value as MSB-first hex (the way prev hashes and
/// targets are conventionally displayed).
fn hex_msb(bytes: &[u8; 32]) -> String {
    let encoded = mining_protocol::hex::encode_block_hash_lower(bytes);
    String::from_utf8(encoded.to_vec()).expect("hex encoder emits ASCII")
}

/// Parse the SV2-spec base58check public key into the raw 32-byte
/// xonly form `Sv2Config::pool_pubkey` expects.
fn parse_pubkey(encoded: &str) -> Result<[u8; 32]> {
    let key = PublicKey::from_str(encoded).map_err(|err| anyhow!("{err:?}"))?;
    Ok(key.into())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let pool = env::var("POOL").context("set POOL=host:port (V2 endpoint)")?;
    let user = env::var("USER").context("set USER=worker.name")?;
    let pool_pubkey = if let Ok(hex) = env::var("POOL_PUBKEY") {
        Some(parse_pubkey(&hex).context("POOL_PUBKEY")?)
    } else {
        eprintln!("[warn] POOL_PUBKEY unset — using ValidateNothing");
        None
    };

    println!("connecting to {pool} as {user}");
    let stream = TcpStream::connect(&pool)
        .await
        .with_context(|| format!("connect {pool}"))?;
    let mut transport = FromTokio::new(stream);

    let config = Sv2Config::new(&user, pool_pubkey)
        .map_err(|err| anyhow!("user identity too long: {err:?}"))?;
    let mut client = StratumV2Client::new(&config, crypto_ctx(), ProbeRng);
    client
        .connect(&mut transport)
        .await
        .map_err(|e| anyhow!("connect: {e:?}"))?;
    println!("[connected] noise + setup-connection + open-channel sent");

    let mut visitor = PrintVisitor::default();
    let mut submitted = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        client
            .drive(&mut transport)
            .await
            .map_err(|e| anyhow!("drive: {e:?}"))?;
        client.accept(&mut visitor);

        if !submitted {
            if let Some(job_id) = visitor.latest_job {
                // Deliberately bad share against a real `JobId`:
                // pool will return `SubmitSharesError`. The point
                // is to exercise the encode + flush path, not to
                // mine.
                let share = Share {
                    id: job_id,
                    nonce: 0,
                    ntime: 0x6044_9522,
                    version_bits: 0,
                    extranonce_2: &[],
                };
                match client.submit(&share) {
                    Ok(()) => println!("[submit] queued"),
                    Err(e) => println!("[submit] error {e:?}"),
                }
                submitted = true;
            }
        }
    }

    if visitor.latest_job.is_none() {
        eprintln!("[warn] timed out without seeing a NewMiningJob");
    }
    println!("done");
    Ok(())
}
