//! Debug Stratum V2 server — minimal pool simulator for development & testing.
//!
//! Runs the server side of the elementary mining flow:
//!
//!   noise NX handshake → SetupConnection → OpenStandardMiningChannel
//!     → NewMiningJob + SetNewPrevHash → SubmitSharesStandard loop
//!
//! Run with `cargo run --example debug_sv2_server -- --help` to see flags.
//! The authority public key (clients should hardcode it for cert
//! validation) is logged on startup.

use std::net::SocketAddr;
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{Read as _, Write as _};
use ii_sv2::framing::{FrameCodec, Header, IntoFrame, MessageGroup};
use ii_sv2::messages::{
    MiningMessages, NewMiningJob, OpenStandardMiningChannelSuccess, SetNewPrevHash,
    SetupConnectionSuccess, SubmitSharesError, SubmitSharesSuccess,
};
use ii_sv2::security::{
    CertificateHeader, CertificateSigningRequest, CertificateWithPrivateKey, CryptoCtx,
    EncryptedCodec, PublicKey, Rng, Secp256k1KeyPair, SecretKey, SecretSecp256k1Key, accept,
    secp256k1,
};
use ii_sv2::types::{Bytes0_32, Str0_255, Uint256};
use rand::RngCore;
use tokio::net::{TcpListener, TcpStream};
use tracing::{Instrument, error, info, info_span};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug, Clone)]
#[command(
    name = "debug-sv2-server",
    about = "Minimal Stratum V2 pool simulator for debugging.",
    long_about = "Minimal Stratum V2 pool simulator for debugging.\n\n\
                  The static authority and server keypairs are derived from \
                  hardcoded private keys baked into the binary, so the \
                  authority public key is stable across runs and is logged on \
                  startup. Clients should hardcode that pubkey when validating \
                  the server certificate."
)]
struct Args {
    /// TCP socket to bind.
    #[arg(long, default_value = "127.0.0.1:3333")]
    bind: SocketAddr,

    /// Per-share difficulty (the channel target is scaled inversely by this).
    #[arg(long, default_value_t = 1.0)]
    difficulty: f64,

    /// Probability that a submitted share is rejected.
    /// 0.0 = accept all, 1.0 = reject all, anything in between = proportional.
    #[arg(long, default_value_t = 0.0)]
    rejection_ratio: f64,
}

// ---------------------------------------------------------------------------
// Static identities — debug only, NOT secure key material.
// ---------------------------------------------------------------------------

const AUTHORITY_PRIVATE_KEY: [u8; 32] = [
    0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe,
    0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe, 0xca, 0xfe,
];
const SERVER_STATIC_PRIVATE_KEY: [u8; 32] = [
    0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
    0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
];

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

static CTX: OnceLock<CryptoCtx<secp256k1::All>> = OnceLock::new();
fn ctx() -> &'static CryptoCtx<secp256k1::All> {
    CTX.get_or_init(CryptoCtx::new)
}

/// `Rng` over the OS CSPRNG. Used both per-session for ephemeral keys and
/// once at startup to mix entropy into the server's elligator encoding.
struct OsRng;
impl Rng for OsRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand::rng().fill_bytes(dest);
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    if !(0.0..=1.0).contains(&args.rejection_ratio) {
        bail!(
            "rejection_ratio must be in [0, 1] (got {})",
            args.rejection_ratio
        );
    }
    if args.difficulty <= 0.0 || !args.difficulty.is_finite() {
        bail!("difficulty must be a positive finite number");
    }

    let cert = build_cert();
    info!(
        authority_pubkey = %cert.authority_public_key,
        server_static_pubkey = %cert.header.remote_static_public_key,
        difficulty = args.difficulty,
        rejection_ratio = args.rejection_ratio,
        "static identity loaded — clients must hardcode the authority pubkey"
    );

    let listener = TcpListener::bind(args.bind).await?;
    info!(bind = %args.bind, "listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        let args = args.clone();
        let span = info_span!("session", %peer);
        tokio::spawn(
            async move {
                if let Err(e) = handle_session(stream, cert, args).await {
                    error!(error = ?e, "session terminated");
                }
            }
            .instrument(span),
        );
    }
}

// ---------------------------------------------------------------------------
// Cert assembly
// ---------------------------------------------------------------------------

fn build_cert() -> CertificateWithPrivateKey {
    let auth_secret = SecretSecp256k1Key::try_from(AUTHORITY_PRIVATE_KEY)
        .expect("hardcoded authority private key out of curve range");
    let server_secret = SecretSecp256k1Key::try_from(SERVER_STATIC_PRIVATE_KEY)
        .expect("hardcoded server private key out of curve range");

    let mut rng = OsRng;
    let auth_kp = Secp256k1KeyPair::from_secret_key(&auth_secret, &mut rng, ctx());
    let server_kp = Secp256k1KeyPair::from_secret_key(&server_secret, &mut rng, ctx());

    let header = CertificateHeader {
        version: 0,
        valid_from: 0,
        not_valid_after: u32::MAX,
        remote_static_public_key: PublicKey::from(&server_kp),
    };
    let signed = CertificateSigningRequest { header }.sign(&auth_kp, ctx().as_secp256k1());

    CertificateWithPrivateKey {
        header: signed.header,
        authority_public_key: signed.authority_public_key,
        server_static_private_key: SecretKey::from(&server_kp),
        signature: signed.signature,
    }
}

// ---------------------------------------------------------------------------
// Per-session driver
// ---------------------------------------------------------------------------

async fn handle_session(
    stream: TcpStream,
    cert: CertificateWithPrivateKey,
    args: Args,
) -> Result<()> {
    info!("connection accepted");
    let mut io = FromTokio::new(stream);
    let session = accept(&mut io, OsRng, ctx(), &cert)
        .await
        .map_err(|e| anyhow!("noise handshake failed: {:?}", e))?;
    let (mut codec, session_id) = EncryptedCodec::from_handshake(session);
    info!(session_id = %session_id, "noise handshake complete");

    let mut hbuf = [0u8; EncryptedCodec::HEADER_WIRE_SIZE];
    let mut pbuf = [0u8; 4096];
    let mut wire = [0u8; 4096];

    // -- Stage 1: SetupConnection --------------------------------------------
    let (header, pn) = recv(&mut io, &mut codec, &mut hbuf, &mut pbuf).await?;
    let frame = codec
        .decode_frame(header, &mut pbuf[..pn])
        .map_err(|e| anyhow!("decode frame: {:?}", e))?;
    match MiningMessages::dispatch(frame).map_err(|e| anyhow!("dispatch: {:?}", e))? {
        MiningMessages::SetupConnection(s) => info!(
            protocol = s.protocol,
            min_version = s.min_version,
            max_version = s.max_version,
            vendor = &*s.device.vendor,
            fw = &*s.device.fw_ver,
            "received SetupConnection"
        ),
        other => bail!("expected SetupConnection, got {}", message_kind(&other)),
    }
    let success = SetupConnectionSuccess {
        used_version: 2,
        flags: 0,
    };
    send(&mut io, &mut codec, &success, &mut wire).await?;
    info!("sent SetupConnectionSuccess");

    // -- Stage 2: OpenStandardMiningChannel ----------------------------------
    let (header, pn) = recv(&mut io, &mut codec, &mut hbuf, &mut pbuf).await?;
    let frame = codec
        .decode_frame(header, &mut pbuf[..pn])
        .map_err(|e| anyhow!("decode frame: {:?}", e))?;
    let request_id =
        match MiningMessages::dispatch(frame).map_err(|e| anyhow!("dispatch: {:?}", e))? {
            MiningMessages::OpenStandardMiningChannel(o) => {
                info!(
                    request_id = o.request_id,
                    user = &*o.user,
                    hashrate = o.nominal_hashrate,
                    "received OpenStandardMiningChannel"
                );
                o.request_id
            }
            other => bail!(
                "expected OpenStandardMiningChannel, got {}",
                message_kind(&other)
            ),
        };

    let target_bytes = compute_target_le(args.difficulty);
    let success = OpenStandardMiningChannelSuccess {
        request_id,
        channel_id: 1,
        target: Uint256::from_le_bytes(&target_bytes),
        extranonce_prefix: Bytes0_32::try_from(&[][..]).unwrap(),
        group_channel_id: 0,
    };
    send(&mut io, &mut codec, &success, &mut wire).await?;
    info!(channel_id = 1, "sent OpenStandardMiningChannelSuccess");

    // -- Stage 3: prime the channel with prev-hash + first job ---------------
    let prev_hash_bytes = [0x42u8; 32];
    let merkle_root_bytes = [0u8; 32];
    let job = NewMiningJob {
        channel_id: 1,
        job_id: 1,
        min_ntime: Some(0),
        version: 0x2000_0000,
        merkle_root: Uint256::from_le_bytes(&merkle_root_bytes),
    };
    let prev = SetNewPrevHash {
        channel_id: 1,
        job_id: 1,
        prev_hash: Uint256::from_le_bytes(&prev_hash_bytes),
        min_ntime: 0,
        nbits: 0x1d00_ffff, // bitcoin difficulty-1 nbits, debug placeholder
    };
    send(&mut io, &mut codec, &job, &mut wire).await?;
    send(&mut io, &mut codec, &prev, &mut wire).await?;
    info!(job_id = 1, "sent NewMiningJob + SetNewPrevHash");

    // -- Stage 4: share-submission loop --------------------------------------
    let mut accepted = 0u32;
    let mut rejected = 0u32;
    loop {
        let (header, pn) = recv(&mut io, &mut codec, &mut hbuf, &mut pbuf).await?;
        let frame = codec
            .decode_frame(header, &mut pbuf[..pn])
            .map_err(|e| anyhow!("decode frame: {:?}", e))?;
        let submit =
            match MiningMessages::dispatch(frame).map_err(|e| anyhow!("dispatch: {:?}", e))? {
                MiningMessages::SubmitSharesStandard(s) => s,
                other => bail!("unexpected message in share loop: {}", message_kind(&other)),
            };

        let reject = args.rejection_ratio > 0.0
            && (args.rejection_ratio >= 1.0 || rand::random::<f64>() < args.rejection_ratio);
        if reject {
            rejected += 1;
            let err = SubmitSharesError {
                channel_id: submit.channel_id,
                sequence_number: submit.sequence_number,
                error_code: Str0_255::try_from(SubmitSharesError::DIFFICULTY_TOO_LOW).unwrap(),
            };
            send(&mut io, &mut codec, &err, &mut wire).await?;
            info!(
                seq = submit.sequence_number,
                job = submit.job_id,
                accepted,
                rejected,
                "share rejected"
            );
        } else {
            accepted += 1;
            let ok = SubmitSharesSuccess {
                channel_id: submit.channel_id,
                last_sequence_number: submit.sequence_number,
                new_submits_accepted_count: 1,
                new_shares_sum: 1,
            };
            send(&mut io, &mut codec, &ok, &mut wire).await?;
            info!(
                seq = submit.sequence_number,
                job = submit.job_id,
                accepted,
                rejected,
                "share accepted"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// IO helpers (encrypted frame in / out)
// ---------------------------------------------------------------------------

async fn recv(
    io: &mut FromTokio<TcpStream>,
    codec: &mut EncryptedCodec,
    hbuf: &mut [u8; EncryptedCodec::HEADER_WIRE_SIZE],
    pbuf: &mut [u8],
) -> Result<(Header, usize)> {
    io.read_exact(hbuf)
        .await
        .map_err(|e| anyhow!("header read: {:?}", e))?;
    let header = codec
        .decode_header(hbuf)
        .map_err(|e| anyhow!("decode header: {:?}", e))?;
    let pn = EncryptedCodec::payload_wire_size(header.msg_length as usize);
    if pn > pbuf.len() {
        bail!("payload {} exceeds buffer {}", pn, pbuf.len());
    }
    io.read_exact(&mut pbuf[..pn])
        .await
        .map_err(|e| anyhow!("payload read: {:?}", e))?;
    Ok((header, pn))
}

async fn send<F: IntoFrame>(
    io: &mut FromTokio<TcpStream>,
    codec: &mut EncryptedCodec,
    msg: &F,
    wire: &mut [u8],
) -> Result<()> {
    let n = codec
        .encode(msg, wire)
        .map_err(|e| anyhow!("encode: {:?}", e))?;
    io.write_all(&wire[..n])
        .await
        .map_err(|e| anyhow!("write: {:?}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

/// Crude difficulty → 256-bit little-endian target. Halves the max target
/// for every doubling of `difficulty`. Not bit-exact bitcoin difficulty,
/// just monotonic and visible in logs.
fn compute_target_le(difficulty: f64) -> [u8; 32] {
    let mut be = [0xffu8; 32];
    let shift = difficulty.log2().clamp(0.0, 255.0) as usize;
    let bytes = (shift / 8).min(32);
    let bits = (shift % 8) as u32;
    for slot in be.iter_mut().take(bytes) {
        *slot = 0;
    }
    if bytes < 32 && bits > 0 {
        be[bytes] >>= bits;
    }
    be.reverse();
    be
}

fn message_kind(m: &MiningMessages<'_>) -> &'static str {
    match m {
        MiningMessages::SetupConnection(_) => "SetupConnection",
        MiningMessages::SetupConnectionSuccess(_) => "SetupConnectionSuccess",
        MiningMessages::SetupConnectionError(_) => "SetupConnectionError",
        MiningMessages::OpenStandardMiningChannel(_) => "OpenStandardMiningChannel",
        MiningMessages::OpenStandardMiningChannelSuccess(_) => "OpenStandardMiningChannelSuccess",
        MiningMessages::OpenMiningChannelError(_) => "OpenMiningChannelError",
        MiningMessages::NewMiningJob(_) => "NewMiningJob",
        MiningMessages::SetNewPrevHash(_) => "SetNewPrevHash",
        MiningMessages::SetTarget(_) => "SetTarget",
        MiningMessages::SubmitSharesStandard(_) => "SubmitSharesStandard",
        MiningMessages::SubmitSharesSuccess(_) => "SubmitSharesSuccess",
        MiningMessages::SubmitSharesError(_) => "SubmitSharesError",
    }
}
