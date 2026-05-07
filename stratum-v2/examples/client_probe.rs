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
//! POOL_PUBKEY=<64 hex chars of the pool's authority public key> \
//!   cargo run --example client_probe -p stratum-v2
//! ```
//!
//! `POOL_PUBKEY` is optional — if unset, the probe falls back to
//! `ValidateNothing`, which accepts any responder certificate
//! (suitable only for test pools you control).

use std::env;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use embedded_io_adapters::tokio_1::FromTokio;
use ii_sv2::security::{secp256k1, CryptoCtx, Rng as Sv2Rng};
use mining_protocol::{MiningClient, MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult};
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
    saw_first_job: bool,
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
        self.saw_first_job = true;
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        println!(
            "[submit-result] id={:?} accepted={} error={:?}",
            result.id, result.accepted, result.error_message,
        );
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
    let mut out = String::new();
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Print a 256-bit value as MSB-first hex (the way prev hashes and
/// targets are conventionally displayed).
fn hex_msb(bytes: &[u8; 32]) -> String {
    let mut out = String::new();
    for byte in bytes.iter().rev() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn parse_pubkey(hex: &str) -> Result<[u8; 32]> {
    if hex.len() != 64 {
        return Err(anyhow!(
            "POOL_PUBKEY must be 64 hex chars (got {})",
            hex.len()
        ));
    }
    let mut out = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let hi = decode_hex_nibble(chunk[0])?;
        let lo = decode_hex_nibble(chunk[1])?;
        out[index] = (hi << 4) | lo;
    }
    Ok(out)
}

fn decode_hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(anyhow!("invalid hex character: {:?}", byte as char)),
    }
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

    let config: &'static Sv2Config = Box::leak(Box::new(Sv2Config {
        user: Box::leak(user.into_boxed_str()),
        pool_pubkey,
    }));

    let mut client = StratumV2Client::new(config, crypto_ctx(), ProbeRng);
    client
        .connect(&mut transport)
        .await
        .map_err(|e| anyhow!("connect: {e:?}"))?;
    println!("[connected] noise + setup-connection + open-channel sent");

    let mut visitor = PrintVisitor::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        client
            .drive(&mut transport)
            .await
            .map_err(|e| anyhow!("drive: {e:?}"))?;
        client.accept(&mut visitor);
    }

    if !visitor.saw_first_job {
        eprintln!("[warn] timed out without seeing a NewMiningJob");
    }
    println!("done");
    Ok(())
}
