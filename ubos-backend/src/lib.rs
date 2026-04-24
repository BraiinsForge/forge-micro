//! Stratum V1 client backend with pluggable solver.
//!
//! `CURRENT_ASSIGNMENT` wraps the in-flight `WorkAssignment` in a
//! `Mutex` so the stratum task can roll `ntime` and `pool_target` in
//! place. `NEW_JOB` signals the first assignment, `SHARE_TARGET` is
//! a `Watch` for mid-job difficulty updates, `CLEAN_JOBS` is the
//! drop-current-work flag, and `SOLUTIONS` is the solver-to-stratum
//! channel.
//!
//! `fake_solver_task` emits one `Solution` per second for wire
//! testing; `cpu_solver_task` scans nonces against the live pool
//! target.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

#[cfg(test)]
#[path = "tests/header.rs"]
mod tests;

use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_futures::select::{select, Either};
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write;
use portable_atomic::Ordering;
use sha2::{Digest, Sha256};
use static_cell::ConstStaticCell;
use stratum_v1_core::{
    encode_authorize, encode_configure, encode_submit, encode_subscribe, parse_boolean_result,
    parse_incoming, parse_subscribe_result, parse_version_rolling_mask, Authorize, Configure,
    LineBuffer, Notification, Rpc, Submit, Subscribe,
};

const LINE_BUFFER_BYTES: usize = 4096;
const TCP_BUFFER_SIZE: usize = 2048;
const ENCODE_BUFFER_SIZE: usize = 512;
const RESULT_COPY_SIZE: usize = 256;

const CONFIGURE_ID: u32 = 1;
const SUBSCRIBE_ID: u32 = 2;
const AUTHORIZE_ID: u32 = 3;
const SUBMIT_ID_START: u32 = 4;

const CONFIGURE_MAP: &[u8] = br#"{"version-rolling":{"mask":"1fffe000","min-bit-count":2}}"#;

const MAX_EXTRANONCE2_SIZE: usize = 16;
const MAX_JOB_ID_CHARS: usize = 32;
const MAX_EXTRANONCE1_HEX_CHARS: usize = 16;
const MAX_PREV_HASH_CHARS: usize = 64;
// Public pools (e.g. Braiins) ship coinbase parts longer than the regtest
// pool does; 1024 fits their current sizes with headroom.
const MAX_COINB1_CHARS: usize = 1024;
const MAX_COINB2_CHARS: usize = 1024;
// Standard max tree depth for current chain heights.
const MAX_MERKLE_BRANCHES: usize = 16;
const MAX_BITS_CHARS: usize = 8;

const SOCKET_TIMEOUT_SECS: u64 = 120;

// Yield frequency matters: the solver runs on the same executor as the
// embassy-net stack, so long stretches between yields starve packet
// processing. At 100 iters the stack gets a scheduling slot roughly
// every 10 ms on rp2350, keeping the W6100 SPI path alive and DNS/NTP
// queries answered without SocketWrite errors creeping in.
const SOLVER_YIELD_INTERVAL: u32 = 100;

// Initial share target before the pool sends its first set_difficulty.
// Two leading zero bytes corresponds to pool diff 1/256^2 (~1.53e-5),
// matching the fork's default mindiff floor. Once the pool pushes a
// real difficulty, `SHARE_TARGET` below gets updated and the solver
// compares hashes against that instead.
const DEFAULT_TARGET: [u8; 32] = {
    let mut target = [0xFFu8; 32];
    // Top two bytes (MSB of 256-bit LE integer) must be zero.
    target[31] = 0x00;
    target[30] = 0x00;
    target
};

// Fake solver pacing: one share per second avoids spamming the pool.
const FAKE_SOLVER_SHARE_DELAY: Duration = Duration::from_secs(1);

// 5-second sample window is a reasonable trade-off between reactivity on
// the display and noise from short hashing bursts.
const HASHRATE_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

// Bitcoin's consensus rule allows a block header `ntime` up to 2 hours ahead
// of the median past time. Pools enforce a similar cap and reject submits
// with a roll larger than this window. Bounding the roller here matches that
// expectation and prevents a stuck stratum task from drifting `ntime` into
// rejection territory.
const NTIME_ROLL_MAX_SECS: u64 = 7_200;

// `ntime` is rolled at 1 Hz. Sub-second rolling offers no extra search space
// on the wire (pools only see whole-second granularity) and burns Mutex locks
// for no gain. One bump per second matches what bosminer and redacted do.
const NTIME_ROLL_INTERVAL: Duration = Duration::from_secs(1);

/// Pool connection parameters. Declare a `static` of this in firmware and pass
/// `&'static PoolConfig` to the task constructors.
pub struct PoolConfig {
    pub host_ip: [u8; 4],
    pub port: u16,
    pub user: &'static str,
    pub password: &'static str,
    pub reconnect_delay: Duration,
}

/// Work handed to a solver. Header fields are in native `u32` form, decoded
/// once by the stratum task from their big-endian wire hex. The merkle root
/// is precomputed so the solver only touches the 80-byte block header.
///
/// Shape intentionally matches what an ASIC chain driver (BM1362/BM1370)
/// needs to push work: base version, version rolling mask, precomputed
/// pool target. `ntime` is mutable across the assignment's lifetime so
/// the stratum task can roll it while the underlying job stays "in stasis"
/// between notifies. `extranonce2` is always zeros today but typed so a
/// future rolling implementation can populate it without reshaping callers.
#[derive(Clone)]
pub struct WorkAssignment {
    pub job_id: heapless::String<MAX_JOB_ID_CHARS>,
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub version: u32,
    pub version_mask: u32,
    pub nbits: u32,
    pub ntime: u32,
    pub pool_target: [u8; 32],
    pub extranonce2: heapless::Vec<u8, MAX_EXTRANONCE2_SIZE>,
}

/// A hash meeting the pool target. Carries every field required to format
/// a `mining.submit`, so the stratum task needs no side table of in-flight
/// work. `version_bits` is the version-rolling delta the solver actually
/// used (zero for the CPU solver, populated by an ASIC driver).
#[derive(Clone)]
pub struct Solution {
    pub job_id: heapless::String<MAX_JOB_ID_CHARS>,
    pub nonce: u32,
    pub ntime: u32,
    pub version_bits: u32,
    pub extranonce2: heapless::Vec<u8, MAX_EXTRANONCE2_SIZE>,
}

/// Mutable state around the currently active assignment. Holding the base
/// `ntime` plus the wall clock at which it was observed lets the roller cap
/// the roll at `NTIME_ROLL_MAX_SECS` without wall-clock sync: we only need
/// *elapsed* time relative to the notify. `None` means no job yet.
struct JobState {
    assignment: WorkAssignment,
    ntime_base: u32,
    wall_at_base: Instant,
}

/// Latest work assignment from the pool. Stratum task overwrites on notify
/// and mutates `pool_target` on `set_difficulty`. Roller bumps `ntime` in
/// place. Solvers snapshot under the lock and release before hashing so the
/// hot path never contends.
static CURRENT_ASSIGNMENT: Mutex<CriticalSectionRawMutex, Option<JobState>> = Mutex::new(None);

/// Fired on every `mining.notify` so solvers can wake from their initial
/// no-job state. After the first job any subsequent update is picked up by
/// re-locking `CURRENT_ASSIGNMENT`, so this signal matters only at startup.
pub static NEW_JOB: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Latest share target pushed by the pool via `mining.set_difficulty`.
/// The solver compares every `sha256d` output against this target and
/// submits the nonce when the hash (interpreted as a 256-bit
/// little-endian integer) is less than or equal to it.
pub static SHARE_TARGET: Watch<CriticalSectionRawMutex, [u8; 32], 2> = Watch::new();

/// Fired when a notify with `clean_jobs=true` arrives so the solver can abort.
pub static CLEAN_JOBS: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Snapshot the current assignment for a solver. Returns `None` until the
/// first notify installs one.
async fn snapshot_assignment() -> Option<WorkAssignment> {
    let guard = CURRENT_ASSIGNMENT.lock().await;
    guard.as_ref().map(|state| state.assignment.clone())
}

/// Solutions found by solver tasks, consumed by the stratum task for submission.
pub static SOLUTIONS: Channel<CriticalSectionRawMutex, Solution, 8> = Channel::new();

/// Running total of hashes computed across all solver iterations. Bumped in
/// batches of `SOLVER_YIELD_INTERVAL` to keep the atomic cost negligible.
pub static TOTAL_HASHES: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);

/// Hashrate averaged over the last sample interval, in hashes per second.
/// Updated by `hashrate_task` every `HASHRATE_SAMPLE_INTERVAL`.
pub static HASHRATE_HZ: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);

/// Version-rolling mask negotiated with the pool. Written once when the
/// `mining.configure` result lands and refreshed whenever the pool sends a
/// `mining.set_version_mask` notification. Stamped into every new
/// `WorkAssignment` so an ASIC driver can program its per-job mask without
/// threading an extra channel.
static CURRENT_VERSION_MASK: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);

static TCP_RX: ConstStaticCell<[u8; TCP_BUFFER_SIZE]> = ConstStaticCell::new([0; TCP_BUFFER_SIZE]);
static TCP_TX: ConstStaticCell<[u8; TCP_BUFFER_SIZE]> = ConstStaticCell::new([0; TCP_BUFFER_SIZE]);

// Extranonce1 and extranonce2_size from the subscribe response. Written once
// per session during the handshake, read during every subsequent notify.
// CriticalSectionRawMutex because stratum_task is the only async user but the
// Mutex API requires a RawMutex that is Send; NoopRawMutex is not Send.
struct SubscribeState {
    extranonce1_hex: heapless::String<MAX_EXTRANONCE1_HEX_CHARS>,
    extranonce2_size: u8,
}

static SUBSCRIBE_STATE: Mutex<CriticalSectionRawMutex, SubscribeState> =
    Mutex::new(SubscribeState {
        extranonce1_hex: heapless::String::new(),
        extranonce2_size: 0,
    });

#[derive(defmt::Format, Debug)]
enum SessionError {
    Connect,
    SocketRead,
    SocketWrite,
    EncodeOverflow,
    LineTooLong,
    HandshakeMissing,
}

/// Stratum V1 client task. Connects, performs the handshake, then drives the
/// notify loop concurrently with share submission. Reconnects on any error.
///
/// The `LineAction::Notify` variant carries ~1.6 KiB of fixed-size decode
/// buffers by design (no alloc); that is also the dominant contributor to
/// the task future's size. MCU stacks are sized for it.
#[allow(clippy::large_futures)]
#[embassy_executor::task]
pub async fn stratum_task(stack: embassy_net::Stack<'static>, config: &'static PoolConfig) -> ! {
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    loop {
        if let Err(error) = run_session(stack, config, rx_buffer, tx_buffer).await {
            warn!("stratum session error: {}", error);
        }
        Timer::after(config.reconnect_delay).await;
    }
}

async fn run_session(
    stack: embassy_net::Stack<'static>,
    config: &'static PoolConfig,
    rx_buffer: &mut [u8; TCP_BUFFER_SIZE],
    tx_buffer: &mut [u8; TCP_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let [addr_a, addr_b, addr_c, addr_d] = config.host_ip;
    let address = embassy_net::IpAddress::v4(addr_a, addr_b, addr_c, addr_d);

    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(SOCKET_TIMEOUT_SECS)));

    socket
        .connect((address, config.port))
        .await
        .map_err(|_| SessionError::Connect)?;

    info!(
        "stratum: connected to {}.{}.{}.{}:{}",
        addr_a, addr_b, addr_c, addr_d, config.port
    );

    let mut line_buffer = LineBuffer::<LINE_BUFFER_BYTES>::new();
    let mut encode_buffer = [0u8; ENCODE_BUFFER_SIZE];

    do_configure(config, &mut socket, &mut line_buffer, &mut encode_buffer).await?;
    do_subscribe(config, &mut socket, &mut line_buffer, &mut encode_buffer).await?;
    do_authorize(config, &mut socket, &mut line_buffer, &mut encode_buffer).await?;

    info!("stratum: handshake complete, entering notify loop");

    let mut submit_id: u32 = SUBMIT_ID_START;

    loop {
        // Prefer reading the socket whenever bytes are already pending.
        // `recv_queue()` is synchronous and returns the count of buffered
        // bytes without touching the async machinery. If we relied only
        // on `select(fill_line_buffer, SOLUTIONS.receive())` the solver
        // wins the select every time a share is queued, and pool acks
        // pile up in the TCP receive buffer until the pool drops us.
        if socket.recv_queue() > 0 {
            fill_line_buffer(&mut socket, &mut line_buffer).await?;
            loop {
                let next = line_buffer
                    .next_line()
                    .map_err(|_| SessionError::LineTooLong)?;
                let Some(line) = next else { break };
                let action = classify_line(line);
                line_buffer.consume();
                dispatch(action).await?;
            }
            continue;
        }

        // Nothing pending on the socket; block on whichever comes next.
        match select(
            fill_line_buffer(&mut socket, &mut line_buffer),
            SOLUTIONS.receive(),
        )
        .await
        {
            Either::First(read_result) => {
                read_result?;

                loop {
                    let next = line_buffer
                        .next_line()
                        .map_err(|_| SessionError::LineTooLong)?;
                    let Some(line) = next else { break };
                    let action = classify_line(line);
                    line_buffer.consume();
                    dispatch(action).await?;
                }
            }
            Either::Second(solution) => {
                send_share(
                    config,
                    &mut socket,
                    &mut encode_buffer,
                    &solution,
                    submit_id,
                )
                .await?;
                info!("stratum: submit sent id={}", submit_id);
                submit_id = submit_id.wrapping_add(1).max(SUBMIT_ID_START);
            }
        }
    }
}

async fn fill_line_buffer(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
) -> Result<(), SessionError> {
    let writable = line_buffer.writable();
    if writable.is_empty() {
        return Err(SessionError::LineTooLong);
    }
    let read_count = socket
        .read(writable)
        .await
        .map_err(|_| SessionError::SocketRead)?;
    if read_count == 0 {
        return Err(SessionError::SocketRead);
    }
    info!("stratum: read {} bytes", read_count);
    line_buffer.advance(read_count);
    Ok(())
}

// Notify carries ~1.6 KiB of fixed arrays. No heap in no_std/no_alloc.
// Not ideal but I'm ok with this.
#[allow(clippy::large_enum_variant)]
enum LineAction {
    Notify {
        job_id: [u8; MAX_JOB_ID_CHARS],
        job_id_len: usize,
        clean_jobs: bool,
        version: [u8; 8],
        version_len: usize,
        time: [u8; 8],
        time_len: usize,
        prev_hash: [u8; MAX_PREV_HASH_CHARS],
        prev_hash_len: usize,
        coinbase_1: [u8; MAX_COINB1_CHARS],
        coinbase_1_len: usize,
        coinbase_2: [u8; MAX_COINB2_CHARS],
        coinbase_2_len: usize,
        merkle_branches: [[u8; 64]; MAX_MERKLE_BRANCHES],
        merkle_depth: u8,
        bits: [u8; MAX_BITS_CHARS],
        bits_len: usize,
    },
    SetDifficulty(f64),
    SetVersionMask(u32),
    OtherNotification,
    Response {
        id: u32,
        accepted: bool,
        is_error: bool,
        stratum_error: heapless::String<128>,
    },
    ParseError,
}

fn classify_line(line: &[u8]) -> LineAction {
    let rpc = match parse_incoming(line) {
        Ok(rpc) => rpc,
        Err(error) => {
            let snippet = &line[..line.len().min(128)];
            warn!(
                "stratum: parse error {} on line ({=[u8]:a}) len={}",
                defmt::Debug2Format(&error),
                snippet,
                line.len()
            );
            return LineAction::ParseError;
        }
    };

    match rpc {
        Rpc::Notification(Notification::Notify(notify)) => {
            let job_id_src = notify.job_id.as_bytes();
            let job_id_len = job_id_src.len().min(MAX_JOB_ID_CHARS);
            let mut job_id = [0u8; MAX_JOB_ID_CHARS];
            job_id[..job_id_len].copy_from_slice(&job_id_src[..job_id_len]);

            let version_src = notify.version.as_bytes();
            let version_len = version_src.len().min(8);
            let mut version = [0u8; 8];
            version[..version_len].copy_from_slice(&version_src[..version_len]);

            let time_src = notify.time.as_bytes();
            let time_len = time_src.len().min(8);
            let mut time = [0u8; 8];
            time[..time_len].copy_from_slice(&time_src[..time_len]);

            let prev_hash_src = notify.prev_hash.as_bytes();
            let prev_hash_len = prev_hash_src.len().min(MAX_PREV_HASH_CHARS);
            let mut prev_hash = [0u8; MAX_PREV_HASH_CHARS];
            prev_hash[..prev_hash_len].copy_from_slice(&prev_hash_src[..prev_hash_len]);

            let coinbase_1_src = notify.coin_base_1.as_bytes();
            let coinbase_1_len = coinbase_1_src.len().min(MAX_COINB1_CHARS);
            let mut coinbase_1 = [0u8; MAX_COINB1_CHARS];
            coinbase_1[..coinbase_1_len].copy_from_slice(&coinbase_1_src[..coinbase_1_len]);

            let coinbase_2_src = notify.coin_base_2.as_bytes();
            let coinbase_2_len = coinbase_2_src.len().min(MAX_COINB2_CHARS);
            let mut coinbase_2 = [0u8; MAX_COINB2_CHARS];
            coinbase_2[..coinbase_2_len].copy_from_slice(&coinbase_2_src[..coinbase_2_len]);

            let bits_src = notify.bits.as_bytes();
            let bits_len = bits_src.len().min(MAX_BITS_CHARS);
            let mut bits = [0u8; MAX_BITS_CHARS];
            bits[..bits_len].copy_from_slice(&bits_src[..bits_len]);

            let mut merkle_branches = [[0u8; 64]; MAX_MERKLE_BRANCHES];
            for (slot, src) in merkle_branches
                .iter_mut()
                .zip(notify.merkle_branch.iter().take(MAX_MERKLE_BRANCHES))
            {
                let branch_src = src.as_bytes();
                let branch_len = branch_src.len().min(64);
                slot[..branch_len].copy_from_slice(&branch_src[..branch_len]);
            }

            #[allow(clippy::cast_possible_truncation)]
            LineAction::Notify {
                job_id,
                job_id_len,
                clean_jobs: notify.clean_jobs,
                merkle_depth: notify.merkle_branch.len().min(MAX_MERKLE_BRANCHES) as u8,
                version,
                version_len,
                time,
                time_len,
                prev_hash,
                prev_hash_len,
                coinbase_1,
                coinbase_1_len,
                coinbase_2,
                coinbase_2_len,
                merkle_branches,
                bits,
                bits_len,
            }
        }
        Rpc::Notification(Notification::SetDifficulty(difficulty)) => {
            LineAction::SetDifficulty(difficulty.0)
        }
        Rpc::Notification(Notification::SetVersionMask(mask)) => {
            LineAction::SetVersionMask(mask.mask)
        }
        Rpc::Notification(_) => LineAction::OtherNotification,
        Rpc::Response(response) => {
            let mut error_str = heapless::String::<128>::new();
            if let Some(err) = &response.stratum_error {
                let _ = write!(error_str, "{err:#?}");
            }
            let accepted = response
                .stratum_result
                .and_then(|bytes| parse_boolean_result(bytes).ok())
                .unwrap_or(false);
            LineAction::Response {
                id: response.id,
                accepted,
                is_error: response.stratum_error.is_some(),
                stratum_error: error_str,
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn dispatch(action: LineAction) -> Result<(), SessionError> {
    match action {
        LineAction::Notify {
            job_id,
            job_id_len,
            clean_jobs,
            coinbase_1_len,
            coinbase_2_len,
            merkle_depth,
            version,
            version_len,
            time,
            time_len,
            prev_hash,
            prev_hash_len,
            coinbase_1,
            coinbase_2,
            merkle_branches,
            bits,
            bits_len,
        } => {
            info!(
                "stratum: notify job_id={=[u8]:a} clean={} cb1={} cb2={} merkle={}",
                &job_id[..job_id_len],
                clean_jobs,
                coinbase_1_len,
                coinbase_2_len,
                merkle_depth
            );

            let mut version_bin = [0u8; 4];
            let mut prev_hash_bin = [0u8; 32];
            let mut time_bin = [0u8; 4];
            let mut bits_bin = [0u8; 4];
            let mut cb1_bin = [0u8; MAX_COINB1_CHARS / 2];
            let mut cb2_bin = [0u8; MAX_COINB2_CHARS / 2];
            let mut en1_bin = [0u8; MAX_EXTRANONCE1_HEX_CHARS / 2];
            // Extranonce2 is always zero-padded; we don't roll it.
            let en2_bin = [0u8; MAX_EXTRANONCE2_SIZE];
            let mut branches_bin = [[0u8; 32]; MAX_MERKLE_BRANCHES];

            let (en1_bin_len, extranonce2_size) = {
                let state = SUBSCRIBE_STATE.lock().await;
                let en1_bytes = state.extranonce1_hex.as_bytes();
                if !decode_hex(en1_bytes, &mut en1_bin[..en1_bytes.len() / 2]) {
                    warn!("stratum: invalid hex in extranonce1, skipping job");
                    return Ok(());
                }
                (en1_bytes.len() / 2, state.extranonce2_size)
            };

            let valid_hex = decode_hex(&version[..version_len], &mut version_bin)
                && decode_hex(&prev_hash[..prev_hash_len], &mut prev_hash_bin)
                && decode_hex(&time[..time_len], &mut time_bin)
                && decode_hex(&bits[..bits_len], &mut bits_bin)
                && decode_hex(
                    &coinbase_1[..coinbase_1_len],
                    &mut cb1_bin[..coinbase_1_len / 2],
                )
                && decode_hex(
                    &coinbase_2[..coinbase_2_len],
                    &mut cb2_bin[..coinbase_2_len / 2],
                );

            if !valid_hex {
                warn!("stratum: invalid hex in notify, skipping job");
                return Ok(());
            }

            for index in 0..(merkle_depth as usize) {
                if !decode_hex(&merkle_branches[index][..64], &mut branches_bin[index]) {
                    warn!("stratum: invalid hex in merkle branch, skipping job");
                    return Ok(());
                }
            }

            let mut merkle_root = [0u8; 32];
            calc_merkle_root(
                &cb1_bin[..coinbase_1_len / 2],
                &en1_bin[..en1_bin_len],
                &en2_bin[..extranonce2_size as usize],
                &cb2_bin[..coinbase_2_len / 2],
                &branches_bin[..(merkle_depth as usize)],
                &mut merkle_root,
            );

            let mut job_id_str: heapless::String<MAX_JOB_ID_CHARS> = heapless::String::new();
            let ascii = core::str::from_utf8(&job_id[..job_id_len]).unwrap_or("");
            let _ = job_id_str.push_str(ascii);

            // Snapshot whatever target the pool has most recently pushed.
            // Mid-job `set_difficulty` updates mutate the field directly in
            // `CURRENT_ASSIGNMENT`; this is the initial value for the job.
            let pool_target = SHARE_TARGET
                .receiver()
                .and_then(|mut receiver| receiver.try_get())
                .unwrap_or(DEFAULT_TARGET);

            let mut extranonce2: heapless::Vec<u8, MAX_EXTRANONCE2_SIZE> = heapless::Vec::new();
            // Resize by pushing zeros; the Vec's len encodes extranonce2_size.
            for _ in 0..(extranonce2_size as usize).min(MAX_EXTRANONCE2_SIZE) {
                let _ = extranonce2.push(0);
            }

            let ntime_base = u32::from_be_bytes(time_bin);
            let assignment = WorkAssignment {
                job_id: job_id_str,
                prev_hash: prev_hash_bin,
                merkle_root,
                version: u32::from_be_bytes(version_bin),
                version_mask: CURRENT_VERSION_MASK.load(Ordering::Relaxed),
                nbits: u32::from_be_bytes(bits_bin),
                ntime: ntime_base,
                pool_target,
                extranonce2,
            };

            {
                let mut guard = CURRENT_ASSIGNMENT.lock().await;
                *guard = Some(JobState {
                    assignment,
                    ntime_base,
                    wall_at_base: Instant::now(),
                });
            }
            NEW_JOB.signal(());

            if clean_jobs {
                CLEAN_JOBS.signal(());
            }
        }
        LineAction::SetDifficulty(difficulty) => {
            info!("stratum: set_difficulty {}", difficulty);
            let target = target_from_diff(difficulty);
            SHARE_TARGET.sender().send(target);
            // Apply the new target to the in-flight assignment too so an
            // ASIC driver (which reads from the assignment alone) picks it
            // up without a second wake signal.
            let mut guard = CURRENT_ASSIGNMENT.lock().await;
            if let Some(state) = guard.as_mut() {
                state.assignment.pool_target = target;
            }
        }
        LineAction::SetVersionMask(version_mask) => {
            info!("stratum: set_version_mask {:#010x}", version_mask);
            CURRENT_VERSION_MASK.store(version_mask, Ordering::Relaxed);
            let mut guard = CURRENT_ASSIGNMENT.lock().await;
            if let Some(state) = guard.as_mut() {
                state.assignment.version_mask = version_mask;
            }
        }
        LineAction::OtherNotification => {
            info!("stratum: other notification");
        }
        LineAction::Response {
            id,
            accepted,
            is_error,
            stratum_error,
        } => {
            if is_error {
                warn!(
                    "stratum: response id={} rejected: {}",
                    id,
                    stratum_error.as_str()
                );
            } else if accepted {
                info!("stratum: response id={} accepted", id);
            } else {
                info!("stratum: response id={} rejected (result=false)", id);
            }
        }
        LineAction::ParseError => {}
    }
    Ok(())
}

async fn send_share(
    config: &'static PoolConfig,
    socket: &mut TcpSocket<'_>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
    solution: &Solution,
    submit_id: u32,
) -> Result<(), SessionError> {
    let mut extranonce2_hex = heapless::String::<{ MAX_EXTRANONCE2_SIZE * 2 }>::new();
    for byte in &solution.extranonce2 {
        let _ = write!(extranonce2_hex, "{byte:02x}");
    }

    let mut nonce_hex = heapless::String::<8>::new();
    let _ = write!(nonce_hex, "{:08x}", solution.nonce);

    let mut time_hex = heapless::String::<8>::new();
    let _ = write!(time_hex, "{:08x}", solution.ntime);

    // CPU solver emits `version_bits = 0`, so today this is always "00000000".
    // An ASIC driver populates the rolled version delta and the pool XORs it
    // with its own base version per the stratum v1 version-rolling extension.
    let mut version_hex = heapless::String::<8>::new();
    let _ = write!(version_hex, "{:08x}", solution.version_bits);

    let submit = Submit {
        user_name: config.user,
        job_id: solution.job_id.as_str(),
        extra_nonce_2: extranonce2_hex.as_str(),
        time: time_hex.as_str(),
        nonce: nonce_hex.as_str(),
        version: version_hex.as_str(),
    };

    let written = encode_submit(&mut encode_buffer[..], submit_id, &submit)
        .map_err(|_| SessionError::EncodeOverflow)?;

    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    Ok(())
}

enum HandshakeLine {
    Skip,
    FoundWithResult(usize),
    FoundNoResult,
}

fn inspect_handshake_line(
    line: &[u8],
    expected_id: u32,
    result_buffer: &mut [u8; RESULT_COPY_SIZE],
) -> HandshakeLine {
    let Ok(rpc) = parse_incoming(line) else {
        return HandshakeLine::Skip;
    };
    let Rpc::Response(response) = rpc else {
        return HandshakeLine::Skip;
    };
    if response.id != expected_id {
        return HandshakeLine::Skip;
    }
    match response.stratum_result {
        None => HandshakeLine::FoundNoResult,
        Some(result_bytes) => {
            let copy_len = result_bytes.len().min(RESULT_COPY_SIZE);
            result_buffer[..copy_len].copy_from_slice(&result_bytes[..copy_len]);
            HandshakeLine::FoundWithResult(copy_len)
        }
    }
}

async fn read_response(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    expected_id: u32,
) -> Result<Option<[u8; RESULT_COPY_SIZE]>, SessionError> {
    let mut result_buffer = [0u8; RESULT_COPY_SIZE];
    loop {
        fill_line_buffer(socket, line_buffer).await?;
        loop {
            let next = line_buffer
                .next_line()
                .map_err(|_| SessionError::LineTooLong)?;
            let Some(line) = next else { break };

            let found = inspect_handshake_line(line, expected_id, &mut result_buffer);
            line_buffer.consume();

            match found {
                HandshakeLine::Skip => {}
                HandshakeLine::FoundWithResult(len) => {
                    let mut out = [0u8; RESULT_COPY_SIZE];
                    out[..len].copy_from_slice(&result_buffer[..len]);
                    return Ok(Some(out));
                }
                HandshakeLine::FoundNoResult => return Ok(None),
            }
        }
    }
}

async fn do_configure(
    _config: &'static PoolConfig,
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let configure = Configure {
        features: &["version-rolling"],
        configure_map: CONFIGURE_MAP,
    };
    let written = encode_configure(&mut encode_buffer[..], CONFIGURE_ID, &configure)
        .map_err(|_| SessionError::EncodeOverflow)?;
    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    // Configure failure is non-fatal; pool may not support it.
    let result_opt = read_response(socket, line_buffer, CONFIGURE_ID).await?;
    if let Some(result_buffer) = result_opt {
        if let Some(mask) = parse_version_rolling_mask(&result_buffer) {
            info!("stratum: configure version-rolling mask {:#010x}", mask);
            CURRENT_VERSION_MASK.store(mask, Ordering::Relaxed);
        }
    }

    Ok(())
}

async fn do_subscribe(
    config: &'static PoolConfig,
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let subscribe = Subscribe {
        agent_signature: Some(config.user),
        extra_nonce_1: None,
    };
    let written = encode_subscribe(&mut encode_buffer[..], SUBSCRIBE_ID, &subscribe)
        .map_err(|_| SessionError::EncodeOverflow)?;
    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    let result_opt = read_response(socket, line_buffer, SUBSCRIBE_ID).await?;
    let result_buffer = result_opt.ok_or(SessionError::HandshakeMissing)?;

    let subscribe_result =
        parse_subscribe_result(&result_buffer).map_err(|_| SessionError::HandshakeMissing)?;

    {
        let mut state = SUBSCRIBE_STATE.lock().await;
        state.extranonce1_hex.clear();
        let truncated_len = subscribe_result.1.len().min(MAX_EXTRANONCE1_HEX_CHARS);
        let _ = state
            .extranonce1_hex
            .push_str(&subscribe_result.1[..truncated_len]);
        state.extranonce2_size = subscribe_result.2;
    }

    info!(
        "stratum: subscribe ok extranonce2_size={}",
        subscribe_result.2
    );

    Ok(())
}

async fn do_authorize(
    config: &'static PoolConfig,
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let authorize = Authorize {
        name: config.user,
        password: config.password,
    };
    let written = encode_authorize(&mut encode_buffer[..], AUTHORIZE_ID, &authorize)
        .map_err(|_| SessionError::EncodeOverflow)?;
    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    let _response = read_response(socket, line_buffer, AUTHORIZE_ID).await?;

    info!("stratum: authorize ok");

    Ok(())
}

/// Fake solver: immediately sends a solution with nonce=0 for every new job.
/// Useful for testing the submit path without burning CPU cycles.
#[embassy_executor::task]
pub async fn fake_solver_task() -> ! {
    loop {
        let Some(assignment) = snapshot_assignment().await else {
            NEW_JOB.wait().await;
            continue;
        };

        info!("solver: new job {}", assignment.job_id.as_str());

        let solution = Solution {
            job_id: assignment.job_id,
            nonce: 0,
            ntime: assignment.ntime,
            version_bits: 0,
            extranonce2: assignment.extranonce2,
        };

        // Channel full means the stratum task is behind; discard rather than block.
        let _ = SOLUTIONS.try_send(solution);

        Timer::after(FAKE_SOLVER_SHARE_DELAY).await;
    }
}

/// CPU solver: hashes the block header continuously until `clean_jobs` fires or
/// a new job arrives. Streams shares for the current job without idling between
/// finds. Switches jobs promptly via non-blocking watch poll each inner iteration.
///
/// Default target starts low enough that even a Cortex-M CPU finds some
/// shares; once the pool sends `set_difficulty`, `SHARE_TARGET` ratchets up.
#[embassy_executor::task]
pub async fn cpu_solver_task() -> ! {
    let mut target_receiver = SHARE_TARGET
        .receiver()
        .expect("BUG: Watch<_, _, 2> supports 2 receivers; tasks <= 2");

    loop {
        let Some(mut assignment) = snapshot_assignment().await else {
            NEW_JOB.wait().await;
            continue;
        };
        let mut nonce_start: u32 = 0;
        let mut target = target_receiver.try_get().unwrap_or(DEFAULT_TARGET);

        info!("solver: new job {}", assignment.job_id.as_str());

        loop {
            // Refresh the snapshot before the next mine_share attempt.
            // A mine_share in flight is not preempted; ntime rolls and
            // non-clean notifies land in the Mutex and are observed on
            // the next iteration.
            if let Some(fresh) = snapshot_assignment().await {
                if fresh.job_id != assignment.job_id {
                    info!("solver: superseded by new job {}", fresh.job_id.as_str());
                    nonce_start = 0;
                }
                assignment = fresh;
            }
            if let Some(updated) = target_receiver.try_changed() {
                target = updated;
            }

            let mine_future = mine_share(
                assignment.version,
                assignment.prev_hash,
                assignment.merkle_root,
                assignment.ntime,
                assignment.nbits,
                nonce_start,
                target,
            );

            match select(mine_future, CLEAN_JOBS.wait()).await {
                Either::First(found_nonce) => {
                    info!(
                        "solver: share queued nonce={:08x} job={}",
                        found_nonce,
                        assignment.job_id.as_str()
                    );

                    let solution = Solution {
                        job_id: assignment.job_id.clone(),
                        nonce: found_nonce,
                        ntime: assignment.ntime,
                        version_bits: 0,
                        extranonce2: assignment.extranonce2.clone(),
                    };

                    // Channel full means the stratum task is behind; discard rather than block.
                    let _ = SOLUTIONS.try_send(solution);

                    nonce_start = found_nonce.wrapping_add(1);
                }
                Either::Second(()) => {
                    info!("solver: cancelled by clean_jobs");
                    break;
                }
            }
        }
    }
}

/// Rolls `ntime` on the in-flight assignment at 1 Hz. The assignment itself
/// stays "in stasis" between notifies: only `ntime` drifts forward so pools
/// keep accepting our shares instead of rejecting them as time-too-old.
/// Rolling is capped at `NTIME_ROLL_MAX_SECS` past the pool-supplied base to
/// stay inside the header-time window the consensus rules allow.
#[embassy_executor::task]
pub async fn ntime_roller_task() -> ! {
    loop {
        Timer::after(NTIME_ROLL_INTERVAL).await;

        let mut guard = CURRENT_ASSIGNMENT.lock().await;
        let Some(state) = guard.as_mut() else {
            continue;
        };

        let elapsed = Instant::now()
            .checked_duration_since(state.wall_at_base)
            .map_or(0, |duration| duration.as_secs().min(NTIME_ROLL_MAX_SECS));
        #[allow(clippy::cast_possible_truncation)]
        let elapsed_u32 = elapsed as u32;
        state.assignment.ntime = state.ntime_base.wrapping_add(elapsed_u32);
    }
}

/// Build the 80-byte block header.
///
/// The block header wants `version`/`ntime`/`nbits` serialized little-endian
/// and `prev_hash` with each 4-byte word's byte order reversed. `merkle_root`
/// is already in internal byte order from our local double-SHA256 so it goes
/// in raw. The caller converts stratum big-endian hex to `u32` once during
/// notify parsing so the hot path only performs byte-layout work.
#[must_use]
pub fn build_header(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 80] {
    let mut header = [0u8; 80];

    header[0..4].copy_from_slice(&version.to_le_bytes());

    // prev_hash: reverse bytes within each 4-byte word.
    for word_index in 0..8 {
        let source = &prev_hash[word_index * 4..word_index * 4 + 4];
        let dest = &mut header[4 + word_index * 4..4 + word_index * 4 + 4];
        dest[0] = source[3];
        dest[1] = source[2];
        dest[2] = source[1];
        dest[3] = source[0];
    }

    header[36..68].copy_from_slice(&merkle_root);
    header[68..72].copy_from_slice(&ntime.to_le_bytes());
    header[72..76].copy_from_slice(&nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

/// Convert a pool-supplied stratum difficulty to a 256-bit little-endian
/// target suitable for byte-by-byte hash comparison.
///
/// `target` = `truediffone` / `diff`, where `truediffone` is
/// `0xFFFF * 2^208`. f64 precision is ~52 bits so the lowest bytes of
/// the target may be slightly off for exotic diffs, but the high bytes
/// that actually decide share acceptance are accurate.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn target_from_diff(diff: f64) -> [u8; 32] {
    if diff <= 0.0 {
        return [0xFFu8; 32];
    }
    // 2^208 as an exact IEEE-754 double (exponent 208 + bias 1023 = 1231).
    // Avoids relying on libm's powi in no_std builds.
    let pow2_208 = f64::from_bits(0x4CF0_0000_0000_0000);
    let truediff_one = 65_535.0_f64 * pow2_208;
    let mut value = truediff_one / diff;
    let mut target = [0u8; 32];
    for byte in &mut target {
        if value < 1.0 {
            break;
        }
        let low = (value % 256.0) as u8;
        *byte = low;
        // Subtract the low byte before dividing so no libm floor() is needed.
        value = (value - f64::from(low)) / 256.0;
    }
    if value >= 1.0 {
        return [0xFFu8; 32];
    }
    target
}

/// Return `true` when `hash` (little-endian 256-bit integer) is less
/// than or equal to `target`. Exact byte-by-byte compare from MSB down.
#[must_use]
pub fn hash_meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for index in (0..32).rev() {
        if hash[index] < target[index] {
            return true;
        }
        if hash[index] > target[index] {
            return false;
        }
    }
    true
}

/// Double-SHA256 of the 80-byte block header produced by `build_header`.
#[must_use]
pub fn header_hash(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 32] {
    let header = build_header(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let mut out = [0u8; 32];
    double_sha256(&header, &mut out);
    out
}

async fn mine_share(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    start_nonce: u32,
    target: [u8; 32],
) -> u32 {
    let mut header = build_header(version, prev_hash, merkle_root, ntime, nbits, start_nonce);

    let mut result_hash = [0u8; 32];
    let mut nonce: u32 = start_nonce;

    loop {
        header[76..80].copy_from_slice(&nonce.to_le_bytes());
        double_sha256(&header, &mut result_hash);

        if hash_meets_target(&result_hash, &target) {
            return nonce;
        }

        nonce = nonce.wrapping_add(1);

        if nonce.is_multiple_of(SOLVER_YIELD_INTERVAL) {
            embassy_futures::yield_now().await;
            TOTAL_HASHES.fetch_add(u64::from(SOLVER_YIELD_INTERVAL), Ordering::Relaxed);
        }
    }
}

/// Periodically computes the current hashrate from `TOTAL_HASHES` and publishes
/// it to `HASHRATE_HZ`. Also logs the rate via defmt.
#[embassy_executor::task]
pub async fn hashrate_task() -> ! {
    let mut last_total: u64 = 0;
    loop {
        Timer::after(HASHRATE_SAMPLE_INTERVAL).await;
        let now_total = TOTAL_HASHES.load(Ordering::Relaxed);
        let delta = now_total.saturating_sub(last_total);
        last_total = now_total;
        // u64 so a future ASIC backend reporting multi-TH/s doesn't
        // saturate the counter.
        let rate = delta / HASHRATE_SAMPLE_INTERVAL.as_secs().max(1);
        HASHRATE_HZ.store(rate, Ordering::Relaxed);
        info!("solver: {} H/s", rate);
    }
}

fn double_sha256(data: &[u8], out: &mut [u8; 32]) {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let first = hasher.finalize();

    let mut hasher2 = Sha256::new();
    hasher2.update(first);
    out.copy_from_slice(&hasher2.finalize());
}

fn calc_merkle_root(
    coinbase_1: &[u8],
    extranonce1: &[u8],
    extranonce2: &[u8],
    coinbase_2: &[u8],
    merkle_branches: &[[u8; 32]],
    root_out: &mut [u8; 32],
) {
    // Serialized coinbase = coinbase_1 + extranonce1 + extranonce2 +
    // coinbase_2. Sized to the worst case of each component's declared
    // limit so raising any of the upstream consts here keeps the buffer
    // in sync.
    let mut coinbase = [0u8; MAX_COINB1_CHARS / 2
        + MAX_EXTRANONCE1_HEX_CHARS / 2
        + MAX_EXTRANONCE2_SIZE
        + MAX_COINB2_CHARS / 2];
    let mut offset = 0;

    for chunk in &[coinbase_1, extranonce1, extranonce2, coinbase_2] {
        coinbase[offset..offset + chunk.len()].copy_from_slice(chunk);
        offset += chunk.len();
    }

    let mut current_hash = [0u8; 32];
    double_sha256(&coinbase[..offset], &mut current_hash);

    let mut branch_concat = [0u8; 64];
    for branch in merkle_branches {
        branch_concat[..32].copy_from_slice(&current_hash);
        branch_concat[32..].copy_from_slice(branch);
        double_sha256(&branch_concat, &mut current_hash);
    }

    root_out.copy_from_slice(&current_hash);
}

fn decode_hex(hex: &[u8], bytes: &mut [u8]) -> bool {
    if hex.len() != bytes.len() * 2 {
        return false;
    }
    for index in 0..bytes.len() {
        let high = hex_nibble(hex[index * 2]);
        let low = hex_nibble(hex[index * 2 + 1]);
        match (high, low) {
            (Some(high_val), Some(low_val)) => bytes[index] = (high_val << 4) | low_val,
            _ => return false,
        }
    }
    true
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
