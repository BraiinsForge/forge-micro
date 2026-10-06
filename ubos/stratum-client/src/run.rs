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
//! The pool-alive bitmap and the reconnect counter below are free
//! functions over private statics in this module. There is one of each
//! per firmware image, so a handle owning them would promise an instance
//! no caller can choose, and the API and the metrics publisher both read
//! them from tasks that hold nothing else.
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
use mining_protocol::{Error, JobId, MiningClient, Share};
use ubos_logging::{info, warn};

/// Pool-index space shared by every per-pool counter keyed by pool
/// index: [`crate::metrics::POOL_METRICS`]. A `u8` pool index fits
/// this space by construction, since the reachability bitmap built
/// from it holds one bit per index.
pub(crate) const POOL_INDEX_SPACE: usize = 8;

use crate::asic_session::PoolHandle;
use crate::state::StratumState;
use crate::work::Solution;

#[cfg(test)]
#[path = "tests/run.rs"]
mod tests;

/// Current per-pool reachability bitmap, one bit per pool index. Set
/// bits render as `Alive` on the dashboard; cleared bits as `Dead`.
/// Pool reachability is a stratum concern, so it is owned here rather
/// than in `ubos-net`. Bits of unconfigured slots stay zero and
/// render `Dead`.
#[must_use]
pub fn pool_alive_bitmap() -> u8 {
    let mut bitmap = 0u8;
    for (pool_index, metrics) in crate::metrics::POOL_METRICS.iter().enumerate() {
        if metrics.snapshot().alive {
            bitmap |= 1u8 << pool_index;
        }
    }
    bitmap
}

/// Maximum `ntime` roll, in seconds, past the pool's published
/// base. Bitcoin's consensus rule allows ~2 hours; pools enforce a
/// similar cap. Bounding the offset here keeps a wedged stratum
/// task from drifting `ntime` into rejection territory.
pub const NTIME_ROLL_MAX_SECS: u32 = 7_200;

/// Run the integration loop forever (until the transport errors).
///
/// Sets the reachability bit of the handle's slot only after
/// `client.connect()` completes the protocol handshake successfully,
/// and clears it on return. A handshake-level failure therefore never
/// appears as a fleeting "alive" state on the dashboard. Callers
/// should invoke [`PoolHandle::record_reconnect_attempt`] at the top
/// of their outer reconnect loop so the counter reflects
/// re-establishment attempts rather than the very first boot
/// connection.
///
/// Empties the slot's work cell on return as well, so the dispatcher
/// stops feeding the chip a dead session's job at once rather than
/// after the caller's reconnect delay.
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
/// rolling `ntime` for an uninterrupted sweep. See
/// [`crate::state::StratumState::publish_work`] for the offset math.
///
/// # Errors
///
/// Surfaces any `Error` returned from `client.connect()` or
/// `client.drive()`. `client.submit()` errors are logged and
/// otherwise swallowed — a single bad submit must not tear down
/// the whole session.
pub async fn run_stratum_client<C, T, const N: usize, const CHAINS: usize>(
    client: &mut C,
    transport: &mut T,
    asic: PoolHandle<'_, N, CHAINS>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    let result =
        run_stratum_client_inner(client, transport, asic, state, ntime_roll_period_secs).await;
    asic.set_alive(false);
    asic.clear_work();
    result
}

/// The session loop proper, wrapped by [`run_stratum_client`].
///
/// The split exists so that every exit path, including the error ones,
/// passes through one place that clears this pool's alive bit and work
/// cell. Returning from here by any route leaves that bookkeeping to the
/// wrapper, so a new `return` added inside cannot forget it.
///
/// The five parameters are peers. The client is the protocol adapter,
/// the transport is the socket, the ASIC handle is where work goes, and
/// the state is the visitor. `main` owns all of them separately.
async fn run_stratum_client_inner<C, T, const N: usize, const CHAINS: usize>(
    client: &mut C,
    transport: &mut T,
    asic: PoolHandle<'_, N, CHAINS>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    client.connect(transport).await?;
    // Handshake completed; only now mark the pool as alive so a
    // failed connect does not produce a transient "alive" blip in the
    // reachability bitmap between this function's entry and the
    // `await?` returning Err.
    asic.set_alive(true);

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
                    state.publish_work(asic, 0);
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
                state.publish_work(asic, 1);
            }
        }
    }
}

/// Look up the outstanding work for `solution.assignment_id`, verify the
/// originating `JobId` is still valid, and submit the share.
/// Stale shares get dropped before they hit the wire.
fn handle_solution<C: MiningClient>(
    client: &mut C,
    state: &mut StratumState,
    solution: Solution,
) -> Result<(), Error> {
    let outstanding = state.work_ring.lookup(solution.assignment_id);
    let Some(outstanding) = outstanding else {
        // Ring rotated past this assignment_id — pool would reject anyway.
        warn!(
            "stratum-client: pool {} solution drop, assignment_id={} not in ring",
            state.slot, solution.assignment_id,
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
