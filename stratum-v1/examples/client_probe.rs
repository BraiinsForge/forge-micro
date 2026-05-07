//! End-to-end smoke test of the new V1 stack against a real pool.
//!
//! Constructs a `StratumV1Client`, drives `connect` + `drive` /
//! `accept` over a tokio `TcpStream` wrapped via
//! `embedded-io-adapters`, and prints every visitor callback.
//! Submits one fake share for the first job to exercise the
//! encode path.
//!
//! Run with:
//!
//! ```sh
//! POOL=stratum.braiins.com:3333 \
//! USER=braiinstest.dumb_worker \
//! PASS=x \
//!   cargo run --example client_probe -p stratum-v1
//! ```
//!
//! Defaults match the published Braiins test pool. The pool side
//! must speak Stratum V1 — `mining.set_difficulty` /
//! `mining.notify`.

use std::env;
use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use embedded_io_adapters::tokio_1::FromTokio;
use mining_protocol::{
    JobId, MiningClient, MiningClientVisitor, MiningJob, SessionUpdate, Share, SubmitResult,
};
use stratum_v1::{StratumV1Client, Sv1Config};
use tokio::net::TcpStream;

const DEFAULT_POOL: &str = "stratum.braiins.com:3333";
const DEFAULT_USER: &str = "braiinstest.dumb_worker";
const DEFAULT_PASS: &str = "x";

#[derive(Default)]
struct PrintVisitor {
    saw_first_job: bool,
}

impl MiningClientVisitor for PrintVisitor {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        println!(
            "[job] id={:?} version={:#010x} mask={:#010x} nbits={:#010x} ntime={:#010x} \
             clean_jobs={} en2_size={}",
            job.id,
            job.version,
            job.version_rolling_mask,
            job.nbits,
            job.ntime,
            job.clean_jobs,
            job.extranonce_2_size,
        );
        if !self.saw_first_job {
            self.saw_first_job = true;
        }
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

fn hex_lsb(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Print a 256-bit little-endian integer as MSB-first hex (the way
/// targets are conventionally written).
fn hex_msb(bytes: &[u8; 32]) -> String {
    let mut out = String::new();
    for byte in bytes.iter().rev() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let pool = env::var("POOL").unwrap_or_else(|_| DEFAULT_POOL.into());
    let user = env::var("USER").unwrap_or_else(|_| DEFAULT_USER.into());
    let pass = env::var("PASS").unwrap_or_else(|_| DEFAULT_PASS.into());
    println!("connecting to {pool} as {user}");

    let stream = TcpStream::connect(&pool)
        .await
        .with_context(|| format!("connect {pool}"))?;
    let mut transport = FromTokio::new(stream);

    // Build a runtime config so we can override via env. The struct
    // takes `&'static str`; leak the strings to satisfy the lifetime.
    let runtime_config: &'static Sv1Config = Box::leak(Box::new(Sv1Config {
        user: Box::leak(user.into_boxed_str()),
        password: Box::leak(pass.into_boxed_str()),
    }));
    let mut client = StratumV1Client::new(runtime_config);
    client
        .connect(&mut transport)
        .await
        .map_err(|e| anyhow!("connect: {e:?}"))?;
    println!("[connected] handshake messages queued");

    let mut visitor = PrintVisitor::default();

    // Run for up to 30 seconds. Submit one fake share once we've
    // seen a job and an extranonce.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut submitted_first_share = false;

    while std::time::Instant::now() < deadline {
        client
            .drive(&mut transport)
            .await
            .map_err(|e| anyhow!("drive: {e:?}"))?;
        let saw_job_before = visitor.saw_first_job;
        client.accept(&mut visitor);
        if !submitted_first_share && !saw_job_before && visitor.saw_first_job {
            // First job just landed — submit a deliberately bad share to
            // exercise the submit encode path. Pool will reject it; we
            // observe the rejection via visit_submit_result.
            let zeros = [0u8; 4];
            let share = Share {
                id: JobId(0),
                nonce: 0,
                ntime: 0x6044_9522,
                version_bits: 0,
                extranonce_2: &zeros,
            };
            match client.submit(&share) {
                Ok(()) => println!("[submit] queued"),
                Err(e) => println!("[submit] error {e:?}"),
            }
            submitted_first_share = true;
        }
    }

    println!("done");
    Ok(())
}
