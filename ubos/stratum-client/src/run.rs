//! `run_stratum_client` — the integration loop.
//!
//! One async function that owns the protocol-side `MiningClient`
//! adapter, the `StratumState` (visitor + rings), and ticks a 1 Hz
//! work generator that publishes fresh `WorkAssignment`s into the
//! `AsicSession`. Solutions coming back from the ASIC are looked
//! up in the `WorkRing`, validated against the `JobRing`, and
//! submitted via the same `MiningClient`.
//!
//! Single-task design: drive / accept / submit / work-generation
//! all multiplex through `embassy_futures::select3`. No mutex over
//! `StratumState` because nothing else holds a borrow.

use embassy_futures::select::{select3, Either3};
use embassy_time::{Duration, Ticker};
use embedded_io_async::{Read, Write};
use heapless::Vec;
use mining_protocol::{Error, MiningClient, Share};
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use ubos_logging::{info, trace, warn};

/// Lifetime-monotonic count of times the stratum reconnect path has
/// been entered AFTER the initial connection attempt. The first attempt
/// after boot is intentionally not counted so a prometheus alert
/// `board_stratum_reconnects_total > 0` only fires when the firmware
/// has actually had to re-establish the link — not on a clean first
/// boot.
///
/// `AtomicU32` (rather than `AtomicU64` like the share counters) is
/// sized for reconnect cadence: a continuous reconnect-once-per-second
/// loop would take ~136 years to overflow 32 bits.
pub static STRATUM_RECONNECTS: AtomicU32 = AtomicU32::new(0);

/// Increment [`STRATUM_RECONNECTS`] on every call past the first.
///
/// Callers invoke this at the top of their outer stratum-reconnect
/// loop. The very first call after boot is a no-op so a successful
/// first connection does not register as a reconnect; every subsequent
/// loop entry adds one to the counter, capturing both TCP-level
/// connect failures and session-level disconnects.
///
/// Caller invariant: only one stratum task per binary calls
/// `record_reconnect_attempt`. The function-local static is
/// process-wide; a second concurrent caller would observe the first
/// caller's `false` and miscount reconnects. If a backup-pool task is
/// ever added, refactor to a per-session counter.
pub fn record_reconnect_attempt() {
    // Caller invariant: only one stratum task per binary calls this
    // function. The static is process-wide; a second concurrent caller
    // would observe the first caller's `false` and miscount reconnects.
    // If a backup-pool task is ever added, refactor to per-session state.
    static FIRST_ATTEMPT: AtomicBool = AtomicBool::new(true);
    if !FIRST_ATTEMPT.swap(false, Ordering::Relaxed) {
        STRATUM_RECONNECTS.fetch_add(1, Ordering::Relaxed);
    }
}

use crate::asic_session::AsicSession;
use crate::state::StratumState;
use crate::work::{OutstandingWork, Solution, WorkAssignment, MAX_EXTRANONCE_2};

/// Index of the primary (and currently only) pool in `POOL_ALIVE_BITMAP`.
/// Bit 0 is the primary pool; a backup-pool task would use bit 1. If a
/// backup pool is ever added, add a `pool_index` parameter to
/// `run_stratum_client` and pass `PRIMARY_POOL_INDEX` from the call site.
const PRIMARY_POOL_INDEX: u8 = 0;

/// Cadence at which the work generator publishes a fresh
/// `WorkAssignment`. Each tick advances `ntime` by one second.
pub const WORK_GENERATOR_PERIOD: Duration = Duration::from_secs(1);

/// Maximum `ntime` roll, in seconds, past the pool's published
/// base. Bitcoin's consensus rule allows ~2 hours; pools enforce a
/// similar cap. Bounding the offset here keeps a wedged stratum
/// task from drifting `ntime` into rejection territory.
pub const NTIME_ROLL_MAX_SECS: u32 = 7_200;

/// Run the integration loop forever (until the transport errors).
///
/// Sets bit 0 of `POOL_ALIVE_BITMAP` only after `client.connect()`
/// completes the protocol handshake successfully, and clears it on
/// return. A handshake-level failure therefore never appears as a
/// fleeting "alive" state on the dashboard. Callers should invoke
/// [`record_reconnect_attempt`] at the top of their outer reconnect
/// loop so the counter reflects re-establishment attempts rather than
/// the very first boot connection.
///
/// Returns `Err` when the underlying `MiningClient` does — the
/// firmware's outer reconnect loop drops `client` and `transport`,
/// builds fresh ones, and calls `run_stratum_client` again.
///
/// # Errors
///
/// Surfaces any `Error` returned from `client.connect()` or
/// `client.drive()`. `client.submit()` errors are logged and
/// otherwise swallowed — a single bad submit must not tear down
/// the whole session.
pub async fn run_stratum_client<C, T>(
    client: &mut C,
    transport: &mut T,
    asic: &'static AsicSession,
    state: &mut StratumState,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    let result = run_stratum_client_inner(client, transport, asic, state).await;
    ubos_net::net::set_pool_alive(PRIMARY_POOL_INDEX, false);
    result
}

async fn run_stratum_client_inner<C, T>(
    client: &mut C,
    transport: &mut T,
    asic: &'static AsicSession,
    state: &mut StratumState,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    client.connect(transport).await?;
    // Handshake completed; only now mark the pool as alive so a
    // failed connect does not produce a transient "alive" blip in
    // `POOL_ALIVE_BITMAP` between this function's entry and the
    // `await?` returning Err.
    ubos_net::net::set_pool_alive(PRIMARY_POOL_INDEX, true);

    let mut work_ticker = Ticker::every(WORK_GENERATOR_PERIOD);

    loop {
        match select3(
            client.drive(transport),
            asic.take_solution(),
            work_ticker.next(),
        )
        .await
        {
            Either3::First(drive_result) => {
                drive_result?;
                client.accept(state);
            }
            Either3::Second(solution) => {
                if let Err(error) = handle_solution(client, state, solution) {
                    warn!("stratum-client: submit dropped error={:?}", error);
                }
            }
            Either3::Third(()) => {
                publish_work(state, asic);
            }
        }
    }
}

/// Look up the outstanding work for `solution.work_id`, verify the
/// originating `JobId` is still valid, and submit the share.
/// Stale shares get dropped before they hit the wire.
fn handle_solution<C: MiningClient>(
    client: &mut C,
    state: &mut StratumState,
    solution: Solution,
) -> Result<(), Error> {
    let outstanding = state.work_ring.lookup(solution.work_id);
    let Some(outstanding) = outstanding else {
        // Ring rotated past this work_id — pool would reject anyway.
        warn!(
            "stratum-client: solution drop, work_id={} not in ring",
            solution.work_id,
        );
        return Ok(());
    };
    if !state.job_ring.is_valid(outstanding.job_id) {
        // clean_jobs invalidated the originating slot.
        warn!(
            "stratum-client: solution drop, job {} invalidated by clean_jobs",
            outstanding.job_id.0,
        );
        return Ok(());
    }

    info!(
        "stratum-client: submit job={} nonce={:#010x} ntime={:#010x}",
        outstanding.job_id.0, solution.nonce, solution.ntime,
    );

    let share = Share {
        id: outstanding.job_id,
        nonce: solution.nonce,
        ntime: solution.ntime,
        version_bits: solution.version_bits,
        extranonce_2: &outstanding.extranonce_2,
    };
    client.submit(&share)
}

/// Build the next `WorkAssignment` from the current ring slot,
/// record an `OutstandingWork` so the eventual `Solution` can be
/// translated back to a `Share`, and publish to the ASIC.
fn publish_work(state: &mut StratumState, asic: &AsicSession) {
    let extranonce_2_size = match state.job_ring.current() {
        Some(slot) if slot.valid => slot.extranonce_2_size,
        _ => return,
    };

    // Roll ntime within the consensus-allowed window.
    if let Some(slot) = state.job_ring.current_mut() {
        if slot.ntime_offset < NTIME_ROLL_MAX_SECS {
            slot.ntime_offset = slot.ntime_offset.saturating_add(1);
        }
    }

    let work_id = state.next_work_id;
    state.next_work_id = state.next_work_id.wrapping_add(1);

    // Adapters reject the session with
    // `SessionRejection::ExtranonceTooLarge` when the pool advertises
    // an `extranonce_2_size` above the static `MAX_EXTRANONCE_2`,
    // so this expect() can only fire on a violated invariant.
    let mut extranonce_2: Vec<u8, MAX_EXTRANONCE_2> = Vec::new();
    extranonce_2
        .resize_default(usize::from(extranonce_2_size))
        .expect("BUG: SetExtranonce validation should have rejected en2_size > MAX_EXTRANONCE_2");

    state.work_ring.put(OutstandingWork {
        work_id,
        job_id: state
            .job_ring
            .current()
            .expect("BUG: current() returned Some above")
            .job_id,
        extranonce_2,
    });

    let slot = state
        .job_ring
        .current()
        .expect("BUG: current() returned Some above");
    let assignment = WorkAssignment {
        work_id,
        prev_hash: slot.prev_hash,
        merkle_root: slot.merkle_root,
        target: slot.target,
        version: slot.version,
        nbits: slot.nbits,
        ntime: slot.ntime_base.wrapping_add(slot.ntime_offset),
    };
    trace!(
        "stratum-client: publish work_id={} job={} ntime={:#010x}",
        assignment.work_id,
        slot.job_id.0,
        assignment.ntime,
    );
    asic.publish_work(assignment);
}
