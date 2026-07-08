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

use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Ticker};
use embedded_io_async::{Read, Write};
use heapless::Vec;
use mining_protocol::{Error, MiningClient, Share};
use portable_atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
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
pub(crate) static STRATUM_RECONNECTS: AtomicU32 = AtomicU32::new(0);

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
use crate::work::{MAX_EXTRANONCE_2, OutstandingWork, Solution, WorkAssignment};

#[cfg(test)]
#[path = "tests/run.rs"]
mod tests;

/// Index of the primary (and currently only) pool in `POOL_ALIVE_BITMAP`.
/// Bit 0 is the primary pool; a backup-pool task would use bit 1. If a
/// backup pool is ever added, add a `pool_index` parameter to
/// `run_stratum_client` and pass `PRIMARY_POOL_INDEX` from the call site.
const PRIMARY_POOL_INDEX: u8 = 0;

/// Per-pool reachability bitmap, one bit per pool index. Set bits render
/// as `Alive` on the dashboard; cleared bits as `Dead`. Pool reachability
/// is a stratum concern, so it is owned here rather than in `ubos-net`.
///
/// Only bit 0 (primary pool) is driven today. The dashboard reserves rows
/// for backup pools; their bits stay zero and render `Dead` until
/// secondary/tertiary stratum clients are wired up.
static POOL_ALIVE_BITMAP: AtomicU8 = AtomicU8::new(0);

/// Current per-pool reachability bitmap. The only path across the crate
/// boundary; the backing atomic stays private.
#[must_use]
pub fn pool_alive_bitmap() -> u8 {
    POOL_ALIVE_BITMAP.load(Ordering::Relaxed)
}

/// Set or clear the reachability bit for `pool_index` (0 = primary).
///
/// Thread-safe; uses a relaxed RMW so the bitmap stays coherent even when
/// multiple tasks race on separate pool indexes.
pub fn set_pool_alive(pool_index: u8, alive: bool) {
    // The debug_assert traps a bad index in tests; the guard keeps an
    // out-of-range shift from writing a bogus bit in release firmware.
    debug_assert!(pool_index < 8, "pool_index must fit in the u8 bitmap");
    if pool_index >= 8 {
        return;
    }
    let bit = 1u8 << pool_index;
    if alive {
        POOL_ALIVE_BITMAP.fetch_or(bit, Ordering::Relaxed);
    } else {
        // `fetch_and` returns the bitmap value before the clear. If the
        // corresponding bit was set, this is a genuine alive→dead transition
        // worth recording. Without this guard the event would fire on every
        // call that already has the bit clear (e.g. the initial boot state
        // before a first successful connect).
        let previous = POOL_ALIVE_BITMAP.fetch_and(!bit, Ordering::Relaxed);
        if previous & bit != 0 {
            crate::events::pool_unreachable();
        }
    }
}

/// Cadence at which the work generator publishes a fresh
/// `WorkAssignment`.
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
/// `ntime_roll_period_secs` configures the per-publish `ntime` roll:
/// `Some(period)` advances the offset by one second every `period`
/// publishes (`period == 0` is clamped to 1); `None` pins `ntime` to
/// the job's base with no roll at all. `Some(1)` reproduces the
/// historical one-second drift, since the publish cadence is
/// [`WORK_GENERATOR_PERIOD`] (1 s) and the elapsed counter advances
/// once per publish — sub-second periods were fake precision. Rolling
/// faster than wall clock drifts `ntime` toward rejection, so callers
/// should not pass a period below 1. See [`publish_work`] for the
/// offset math.
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
    ntime_roll_period_secs: Option<u32>,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    let result =
        run_stratum_client_inner(client, transport, asic, state, ntime_roll_period_secs).await;
    set_pool_alive(PRIMARY_POOL_INDEX, false);
    result
}

async fn run_stratum_client_inner<C, T>(
    client: &mut C,
    transport: &mut T,
    asic: &'static AsicSession,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
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
    set_pool_alive(PRIMARY_POOL_INDEX, true);

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
                publish_work(state, asic, ntime_roll_period_secs);
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
///
/// `ntime_roll_period_secs` is `None` to pin `ntime` to the slot's base
/// (`ntime_offset` stays 0), or `Some(period)` to roll one second
/// forward every `period` publishes of accumulated publish cadence,
/// capped at [`NTIME_ROLL_MAX_SECS`]. A `period` of 0 is clamped to 1
/// rather than treated as `None`, since a caller passing `Some(0)`
/// most likely meant "roll as fast as possible", not "don't roll".
fn publish_work(state: &mut StratumState, asic: &AsicSession, ntime_roll_period_secs: Option<u32>) {
    let extranonce_2_size = match state.job_ring.current() {
        Some(slot) if slot.valid => slot.extranonce_2_size,
        _ => return,
    };

    if let Some(period) = ntime_roll_period_secs
        && let Some(slot) = state.job_ring.current_mut()
    {
        let period = period.max(1);
        slot.roll_elapsed_secs = slot.roll_elapsed_secs.saturating_add(1);
        slot.ntime_offset = (slot.roll_elapsed_secs / period).min(NTIME_ROLL_MAX_SECS);
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
        assignment.work_id, slot.job_id.0, assignment.ntime,
    );
    asic.publish_work(assignment);
}
