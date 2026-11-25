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
//! no caller can choose, and the API reads them from tasks that hold
//! nothing else.
//!
//! One async function that owns the protocol-side `MiningClient`
//! adapter, the `StratumState` visitor with its two rings, and a work
//! generator that publishes fresh `WorkAssignment`s into the
//! `AsicSession`, either on a configurable ticker or once per accepted
//! job when the ticker is disabled. Solutions coming back from the
//! ASIC are looked up in the `WorkRing`, validated against the
//! `JobRing`, and submitted via the same `MiningClient`.
//!
//! Single-task design: drive / accept / submit / work-generation and
//! the session extension's wake all multiplex through
//! `embassy_futures::select4`. No mutex over `StratumState` because
//! nothing else holds a borrow.

use embassy_futures::select::{Either4, select4};
use embassy_time::{Duration, Ticker, Timer};
use embedded_io_async::{Read, Write};
use mining_protocol::{
    DriveEvent, Error, ExtensionFrame, JobId, MiningClient, MiningClientVisitor, MiningJob,
    SessionUpdate, Share, SubmitResult,
};
use ubos_logging::{info, warn};

/// Pool-index space of [`crate::metrics::PoolTable`]. A `u8` pool index
/// fits it by construction, since the reachability bitmap holds one bit
/// per index.
pub(crate) const POOL_INDEX_SPACE: usize = 8;

use crate::asic_session::PoolHandle;
use crate::extension::SessionExtension;
use crate::state::StratumState;
use crate::work::Solution;

#[cfg(test)]
#[path = "tests/run.rs"]
mod tests;

/// Maximum `ntime` roll, in seconds, past the pool's published base:
/// Bitcoin's consensus rule allows up to 2 hours, and pools enforce a
/// similar cap. Bounding the offset here keeps a wedged stratum task
/// from drifting `ntime` into rejection territory.
pub const NTIME_ROLL_MAX_SECS: u32 = 7_200;

/// Run the integration loop until the transport errors.
///
/// Sets the reachability bit of the handle's slot once the pool has
/// handed the session a job, and clears it on return: a session with no
/// job cannot mine, so a handshake the pool then rejects never shows as
/// alive. Callers invoke [`PoolHandle::record_reconnect_attempt`] at the
/// top of their reconnect loop.
///
/// Empties the slot's work cell on return as well, so the dispatcher
/// stops feeding the chip a dead session's job at once rather than
/// after the caller's reconnect delay.
///
/// Returns `Err` when the underlying `MiningClient` does — the
/// firmware's outer reconnect loop drops `client` and `transport`,
/// builds fresh ones, and calls `run_stratum_client` again.
///
/// `ntime_roll_period_secs` doubles as the work-generator cadence.
/// `Some(period)` ticks a [`Ticker`] every `period` seconds, clamped
/// to a minimum of 1 since a caller passing `Some(0)` most likely
/// means publish as fast as possible rather than don't roll.
///
/// Each tick advances the `ntime` offset by exactly one second: the
/// period spaces the publishes without scaling the step, so a period
/// above one second rolls `ntime` slower than wall clock, never
/// faster.
///
/// `None` runs no ticker at all: the work generator instead publishes
/// exactly once per job, right after the job is accepted, with no
/// roll. Re-publishing restarts whatever sweep the ASIC was mid-way
/// through, so a chip that never finishes a sweep never searches past
/// one period's coverage.
///
/// See [`crate::state::StratumState::publish_work`] for the offset
/// math.
///
/// `extension` is the protocol riding the session beside the mining
/// traffic, [`crate::NoExtension`] when there is none. It is told when
/// the session connects and as which user, handed every frame of
/// another extension, and woken at its own deadline; the frame it
/// holds is offered to the adapter after any pending share until the
/// adapter takes it.
///
/// # Errors
///
/// Surfaces any `Error` returned from `client.connect()` or
/// `client.drive()`, and fatal `client.send_extension()` errors.
/// A busy outbound slot retains the pending solution. Other submit
/// errors are logged and discard that solution.
pub async fn run_stratum_client<C, T, const N: usize, const CHAINS: usize>(
    client: &mut C,
    transport: &mut T,
    asic: PoolHandle<'_, N, CHAINS>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
    extension: &mut dyn SessionExtension,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    let result = run_stratum_client_inner(
        client,
        transport,
        asic,
        state,
        ntime_roll_period_secs,
        extension,
    )
    .await;
    asic.set_alive(false);
    asic.clear_work();
    asic.clear_difficulty();
    result
}

/// The session loop proper, wrapped by [`run_stratum_client`].
///
/// The split exists so that every exit path, including the error ones,
/// passes through one place that clears this pool's alive bit, work
/// cell and difficulty. Returning from here by any route leaves that
/// bookkeeping to the wrapper, so a new `return` added inside cannot
/// forget it.
///
/// The six parameters are peers: the client is the protocol adapter,
/// the transport is the socket, the ASIC handle is where work goes, the
/// state is the visitor, and the extension is the protocol riding the
/// session, if any. The caller owns each separately.
async fn run_stratum_client_inner<C, T, const N: usize, const CHAINS: usize>(
    client: &mut C,
    transport: &mut T,
    asic: PoolHandle<'_, N, CHAINS>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
    extension: &mut dyn SessionExtension,
) -> Result<(), Error>
where
    C: MiningClient,
    T: Read + Write,
{
    // `PoolHandle::record_reconnect_attempt` pushed the counters as they
    // stand now, so only a later change needs a push from this loop.
    let mut pushed = asic.counters();
    client.connect(transport).await?;
    extension.connected(client.user());

    let period_secs = ntime_roll_period_secs.map(|period| period.max(1));
    let mut work_ticker =
        period_secs.map(|period| Ticker::every(Duration::from_secs(u64::from(period))));

    let mut last_published: Option<JobId> = None;
    let mut pending_solution = None;

    loop {
        pending_solution = pending_solution.or_else(|| asic.try_take_solution());
        if let Some(solution) = pending_solution {
            match handle_solution(client, state, solution) {
                Err(Error::Busy) => {}
                result => {
                    pending_solution = None;
                    if let Err(error) = result {
                        warn!(
                            "stratum-client: pool {} submit dropped error={:?}",
                            state.slot, error,
                        );
                    }
                }
            }
        }
        if pending_solution.is_none() {
            offer_outbound(client, extension)?;
        }
        let waiting = extension.outbound().is_some();
        let wake_at = extension.deadline().filter(|_| !waiting);

        let tick = async {
            match work_ticker.as_mut() {
                Some(ticker) => ticker.next().await,
                None => core::future::pending::<()>().await,
            }
        };
        let wake = async {
            match wake_at {
                Some(at) => Timer::at(at).await,
                None => core::future::pending::<()>().await,
            }
        };
        let solution = async {
            if pending_solution.is_some() {
                core::future::pending::<()>().await;
            }
            asic.take_solution().await
        };

        match select4(client.drive(transport), solution, tick, wake).await {
            Either4::First(drive_result) => {
                if drive_result? == DriveEvent::OutboundFlushed {
                    continue;
                }
                client.accept(&mut SessionVisitor { state, extension });
                asic.set_alive(state.job_ring.current().is_some());
                if asic.counters() != pushed {
                    pushed = asic.push_counters();
                }
                if work_ticker.is_none()
                    && let Some(current_job_id) = state.job_ring.current().map(|slot| slot.job_id)
                    && last_published != Some(current_job_id)
                {
                    state.publish_work(asic, 0);
                    last_published = Some(current_job_id);
                }
            }
            Either4::Second(solution) => {
                pending_solution = Some(solution);
            }
            Either4::Third(()) => {
                state.publish_work(asic, 1);
            }
            Either4::Fourth(()) => {
                extension.tick();
            }
        }
    }
}

/// Offer the frame the extension holds to the adapter's outbound slot.
/// A slot in use keeps the frame waiting for the next flush; a frame
/// the adapter can never take, because its protocol carries none or
/// the payload is too large, is dropped so the loop does not retry it
/// forever.
fn offer_outbound<C: MiningClient>(
    client: &mut C,
    extension: &mut dyn SessionExtension,
) -> Result<(), Error> {
    let Some(frame) = extension.outbound() else {
        return Ok(());
    };
    match client.send_extension(&frame) {
        Err(Error::Busy) => Ok(()),
        result => {
            if let Err(error) = result {
                warn!(
                    "stratum-client: extension frame dropped ext={:#x} type={:#x} error={:?}",
                    frame.extension_type, frame.msg_type, error,
                );
            }
            extension.outbound_result(result);
            match result {
                Err(Error::NotImplemented | Error::BufferTooSmall) => Ok(()),
                other => other,
            }
        }
    }
}

/// The visitor the loop hands to `accept`: mining events reach the
/// state, frames of another extension reach the extension. It exists
/// because `accept` takes one visitor and the two receivers are
/// separate objects.
struct SessionVisitor<'a> {
    state: &'a mut StratumState,
    extension: &'a mut dyn SessionExtension,
}

impl MiningClientVisitor for SessionVisitor<'_> {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        self.state.visit_new_job(job);
    }

    fn visit_submit_result(&mut self, result: SubmitResult<'_>) {
        self.state.visit_submit_result(result);
    }

    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        self.state.visit_session_update(update);
    }

    fn visit_extension_frame(&mut self, frame: ExtensionFrame<'_>) {
        self.extension.frame(frame);
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
