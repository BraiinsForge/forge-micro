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

//! `run_stratum_client` — the integration loop.
//!
//! One async function that owns the protocol-side `MiningClient`
//! adapter, the `StratumState` (visitor + rings), and a work generator
//! that publishes fresh `WorkAssignment`s into the `AsicSession` —
//! either on a configurable ticker, or once per accepted job when the
//! ticker is disabled. Solutions coming back from the ASIC are looked
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
use mining_protocol::{Error, JobId, MiningClient, Share};
use portable_atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use ubos_logging::{info, trace, warn};

/// Pool-index space shared by the alive bitmap and every per-pool
/// array keyed by pool index: [`STRATUM_RECONNECTS`] here, and
/// `SHARES_ACCEPTED` / `SHARES_REJECTED` in `state.rs`. A `u8` pool
/// index fits this space by construction, since the bitmap holds one
/// bit per index.
pub(crate) const POOL_INDEX_SPACE: usize = 8;

/// Lifetime-monotonic count of times the stratum reconnect path has
/// been entered AFTER the initial connection attempt, one counter per
/// pool slot. The first attempt after boot is intentionally not
/// counted so a prometheus alert `board_stratum_reconnects_total > 0`
/// only fires when the firmware has actually had to re-establish the
/// link — not on a clean first boot.
///
/// `AtomicU32` (rather than `AtomicU64` like the share counters) is
/// sized for reconnect cadence: a continuous reconnect-once-per-second
/// loop would take ~136 years to overflow 32 bits.
pub(crate) static STRATUM_RECONNECTS: [AtomicU32; POOL_INDEX_SPACE] =
    [const { AtomicU32::new(0) }; POOL_INDEX_SPACE];

/// Increment the given pool's slot in [`STRATUM_RECONNECTS`] on every
/// call past the first for that pool.
///
/// Each pool session task invokes this at the top of its outer
/// reconnect loop with its own `pool_index` (0-based, matching
/// [`set_pool_alive`]). The very first call after boot is a no-op per
/// pool so a successful first connection does not register as a
/// reconnect; every subsequent loop entry adds one to that slot's
/// counter, capturing both TCP-level connect failures and
/// session-level disconnects. Out-of-range indexes are ignored, like
/// in [`set_pool_alive`].
pub fn record_reconnect_attempt(pool_index: u8) {
    // Sized to the `POOL_ALIVE_BITMAP` index space so the two per-pool
    // accounting paths stay bounds-consistent.
    static FIRST_ATTEMPT: [AtomicBool; POOL_INDEX_SPACE] =
        [const { AtomicBool::new(true) }; POOL_INDEX_SPACE];
    debug_assert!(
        usize::from(pool_index) < POOL_INDEX_SPACE,
        "pool_index must fit the pool-index space"
    );
    let Some(first_attempt) = FIRST_ATTEMPT.get(usize::from(pool_index)) else {
        return;
    };
    if !first_attempt.swap(false, Ordering::Relaxed) {
        STRATUM_RECONNECTS[usize::from(pool_index)].fetch_add(1, Ordering::Relaxed);
    }
}

use crate::asic_session::PoolHandle;
use crate::state::StratumState;
use crate::work::{MAX_EXTRANONCE_2, OutstandingWork, Solution, WorkAssignment};

#[cfg(test)]
#[path = "tests/run.rs"]
mod tests;

/// Per-pool reachability bitmap, one bit per pool index. Set bits render
/// as `Alive` on the dashboard; cleared bits as `Dead`. Pool reachability
/// is a stratum concern, so it is owned here rather than in `ubos-net`.
///
/// Each pool session task drives its own bit, indexed by its
/// `PoolHandle` slot. Bits of unconfigured slots stay zero and render
/// `Dead`.
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
    debug_assert!(
        usize::from(pool_index) < POOL_INDEX_SPACE,
        "pool_index must fit the pool-index space"
    );
    if usize::from(pool_index) >= POOL_INDEX_SPACE {
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

/// Maximum `ntime` roll, in seconds, past the pool's published
/// base. Bitcoin's consensus rule allows ~2 hours; pools enforce a
/// similar cap. Bounding the offset here keeps a wedged stratum
/// task from drifting `ntime` into rejection territory.
pub const NTIME_ROLL_MAX_SECS: u32 = 7_200;

/// Run the integration loop forever (until the transport errors).
///
/// Sets the `POOL_ALIVE_BITMAP` bit of the handle's slot only after
/// `client.connect()` completes the protocol handshake successfully,
/// and clears it on return. A handshake-level failure therefore never
/// appears as a fleeting "alive" state on the dashboard. Callers
/// should invoke [`record_reconnect_attempt`] at the top of their
/// outer reconnect loop so the counter reflects re-establishment
/// attempts rather than the very first boot connection.
///
/// Returns `Err` when the underlying `MiningClient` does — the
/// firmware's outer reconnect loop drops `client` and `transport`,
/// builds fresh ones, and calls `run_stratum_client` again.
///
/// `ntime_roll_period_secs` doubles as the work-generator cadence:
/// `Some(period)` ticks a [`Ticker`] every `period` seconds (`period
/// == 0` is clamped to 1, since a caller passing `Some(0)` most likely
/// means "publish as fast as possible", not "don't roll") and advances
/// the `ntime` offset by exactly one second per tick — the period
/// spaces the publishes, it does not scale the step, so a period
/// above one second rolls `ntime` slower than wall clock, never
/// faster (and never toward future-timestamp rejection). `Some(1)`
/// reproduces the historical one-second cadence and drift.
///
/// `None` runs no ticker at all: the work generator instead publishes
/// exactly once per job, right after the job is accepted, with no
/// roll. Repeatedly re-publishing the same job — even to hand the chip
/// an updated `ntime` — restarts whatever nonce/version sweep the ASIC
/// was mid-way through, so a chip that never completes a full sweep
/// pass can never search past one period's coverage. `None` trades the
/// rolling `ntime` for an uninterrupted sweep. See [`publish_work`]
/// for the offset math.
///
/// # Errors
///
/// Surfaces any `Error` returned from `client.connect()` or
/// `client.drive()`. `client.submit()` errors are logged and
/// otherwise swallowed — a single bad submit must not tear down
/// the whole session.
pub async fn run_stratum_client<C, T, const N: usize>(
    client: &mut C,
    transport: &mut T,
    asic: PoolHandle<'_, N>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    let result =
        run_stratum_client_inner(client, transport, asic, state, ntime_roll_period_secs).await;
    set_pool_alive(pool_index(&asic), false);
    result
}

/// Bitmap index of the handle's slot. Slots past the bitmap saturate
/// to `u8::MAX`, which [`set_pool_alive`] and
/// [`record_reconnect_attempt`] ignore via their range guards.
fn pool_index<const N: usize>(asic: &PoolHandle<'_, N>) -> u8 {
    u8::try_from(asic.slot()).unwrap_or(u8::MAX)
}

async fn run_stratum_client_inner<C, T, const N: usize>(
    client: &mut C,
    transport: &mut T,
    asic: PoolHandle<'_, N>,
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
    set_pool_alive(pool_index(&asic), true);

    // A caller passing `Some(0)` most likely means "publish as fast as
    // possible", not "don't roll" — that's what `None` is for.
    let period_secs = ntime_roll_period_secs.map(|period| period.max(1));
    let mut work_ticker =
        period_secs.map(|period| Ticker::every(Duration::from_secs(u64::from(period))));

    // Most recently published job, so the `None` (no ticker) path can
    // detect a fresh job and publish exactly once for it, without a
    // ticker driving repeat publishes that would restart the ASIC's
    // sweep.
    let mut last_published: Option<JobId> = None;

    loop {
        let tick = async {
            match work_ticker.as_mut() {
                Some(ticker) => ticker.next().await,
                None => core::future::pending::<()>().await,
            }
        };

        match select3(client.drive(transport), asic.take_solution(), tick).await {
            Either3::First(drive_result) => {
                drive_result?;
                client.accept(state);
                if work_ticker.is_none()
                    && let Some(current_job_id) = state.job_ring.current().map(|slot| slot.job_id)
                    && last_published != Some(current_job_id)
                {
                    // No ticker exists to drive periodic publishes, so a
                    // freshly accepted job would otherwise never reach
                    // the ASIC. One publish per job is exactly what an
                    // uninterrupted sweep needs: the assignment carries
                    // the job's base ntime and the chip sweeps it
                    // without ever being re-published.
                    publish_work(state, asic, 0);
                    last_published = Some(current_job_id);
                }
            }
            Either3::Second(solution) => {
                if let Err(error) = handle_solution(client, state, solution) {
                    warn!(
                        "stratum-client: pool {} submit dropped error={:?}",
                        state.slot, error,
                    );
                }
            }
            Either3::Third(()) => {
                // Each publish is one ntime step regardless of how far
                // apart the ticker spaces them; a period above one
                // second therefore rolls ntime slower than wall clock,
                // never faster.
                publish_work(state, asic, 1);
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
            "stratum-client: pool {} solution drop, work_id={} not in ring",
            state.slot, solution.work_id,
        );
        return Ok(());
    };
    if !state.job_ring.is_valid(outstanding.job_id) {
        // clean_jobs invalidated the originating slot.
        warn!(
            "stratum-client: pool {} solution drop, job {} invalidated by clean_jobs",
            state.slot, outstanding.job_id.0,
        );
        return Ok(());
    }

    info!(
        "stratum-client: pool {} submit job={} nonce={:#010x} ntime={:#010x}",
        state.slot, outstanding.job_id.0, solution.nonce, solution.ntime,
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
/// translated back to a `Share`, and publish into the handle's slot.
///
/// `step` advances the slot's `ntime_offset` by that many seconds,
/// capped at [`NTIME_ROLL_MAX_SECS`], before publishing. The loop in
/// [`run_stratum_client_inner`] passes the roll period on each ticker
/// tick, or `0` for the one-shot publish that hands a freshly accepted
/// job to the ASIC when no ticker is running.
fn publish_work<const N: usize>(state: &mut StratumState, asic: PoolHandle<'_, N>, step: u32) {
    let extranonce_2_size = match state.job_ring.current() {
        Some(slot) if slot.valid => slot.extranonce_2_size,
        _ => return,
    };

    if let Some(slot) = state.job_ring.current_mut() {
        slot.ntime_offset = slot
            .ntime_offset
            .saturating_add(step)
            .min(NTIME_ROLL_MAX_SECS);
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
        version_mask: state.version_mask,
    };
    trace!(
        "stratum-client: publish work_id={} job={} ntime={:#010x}",
        assignment.work_id, slot.job_id.0, assignment.ntime,
    );
    asic.publish_work(assignment);
}
