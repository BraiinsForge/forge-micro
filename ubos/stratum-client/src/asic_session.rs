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

//! `AsicSession` — the polled-cell contract between the host-side
//! stratum sessions and whatever drives the ASIC.
//!
//! One work/solution channel pair per pool slot. `work[slot]` is a
//! `Watch<WorkAssignment>`: each session writes its latest assignment
//! unconditionally (latest-wins), and the ASIC dispatcher decides at
//! dispatch time which slot's cell to read. That decision lives on
//! the consumer side, in `ubos-mining`'s scheduler: a share-ratio
//! policy fed back by difficulty-1 shares picks the slot for each
//! work tick. `solutions[slot]` is a `Channel<Solution>`: the ASIC
//! routes each found solution back to the slot whose work produced
//! it, so a session still receives and submits its in-flight
//! solutions after the scheduler moves on to another slot. Sessions
//! access their pair through a [`PoolHandle`] and never learn which
//! slot the scheduler is currently reading.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{DynReceiver, Watch};

use crate::work::{Solution, WorkAssignment};

#[cfg(test)]
#[path = "tests/asic_session.rs"]
mod tests;

/// Channel depth for solutions, per slot. ASIC mocks send when found;
/// the host drains in its `select!` between `client.drive()` and
/// `PoolHandle::take_solution`. Eight slots is far more headroom
/// than a CPU mock will produce between polls; an ASIC driver may
/// want to tune.
pub const SOLUTION_QUEUE_DEPTH: usize = 8;

/// Per-slot `Watch` capacity: one reader (the ASIC dispatcher) plus
/// one spare. Senders do not consume receiver slots.
const WATCH_RECEIVERS: usize = 2;

/// Polled host↔ASIC handoff with one channel pair per pool slot.
///
/// `N` is the pool-slot capacity. It defaults to 1 so single-pool
/// consumers (host tooling, tests) can keep writing plain
/// `AsicSession`; firmware instantiates the config-sized alias from
/// `ubos-firmware`. `static` lifetime is the intended use: the
/// firmware allocates one in a `static` and passes `&'static
/// AsicSession<N>` to both the host-side stratum tasks (as
/// [`PoolHandle`]s) and the ASIC-side worker.
pub struct AsicSession<const N: usize = 1> {
    work: [Watch<CriticalSectionRawMutex, WorkAssignment, WATCH_RECEIVERS>; N],
    solutions: [Channel<CriticalSectionRawMutex, Solution, SOLUTION_QUEUE_DEPTH>; N],
}

impl<const N: usize> AsicSession<N> {
    #[must_use]
    pub const fn new() -> Self {
        const { assert!(N > 0, "AsicSession needs at least one slot") };
        Self {
            work: [const { Watch::new() }; N],
            solutions: [const { Channel::new() }; N],
        }
    }

    // ---- Host side ----

    /// Per-slot face of the session, handed to the stratum task that
    /// owns `slot`.
    ///
    /// # Panics
    ///
    /// Panics if `slot` is not below `N`.
    #[must_use]
    pub fn pool_handle(&self, slot: usize) -> PoolHandle<'_, N> {
        assert!(slot < N, "pool slot out of range");
        PoolHandle {
            session: self,
            slot,
        }
    }

    // ---- ASIC side ----

    /// Receiver handle for one slot's work cell. Returns `None` if
    /// the `Watch`'s receiver budget is exhausted (won't happen in
    /// practice with `WATCH_RECEIVERS = 2` and one ASIC consumer per
    /// slot).
    ///
    /// # Panics
    ///
    /// Panics if `slot` is not below `N`.
    #[must_use]
    pub fn work_receiver(&self, slot: usize) -> Option<DynReceiver<'_, WorkAssignment>> {
        self.work[slot].dyn_receiver()
    }

    /// Push a found solution into `slot`'s channel — the slot that
    /// dispatched the originating work, not necessarily the slot the
    /// scheduler currently reads. Returns `false` on full — matches
    /// the "tiny FIFO on the chip" model where the host is expected
    /// to drain promptly. Callers may log or retry; we don't surface
    /// a richer error type because the only failure mode is the
    /// queue being full.
    ///
    /// # Panics
    ///
    /// Panics if `slot` is not below `N`.
    pub fn submit_solution(&self, slot: usize, solution: Solution) -> bool {
        self.solutions[slot].try_send(solution).is_ok()
    }
}

impl<const N: usize> Default for AsicSession<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// One pool slot's face of the [`AsicSession`] — the only view a
/// stratum session task gets. `Copy`: the firmware creates one per
/// session task from the shared `&'static AsicSession<N>`.
#[derive(Clone, Copy)]
pub struct PoolHandle<'a, const N: usize> {
    session: &'a AsicSession<N>,
    slot: usize,
}

impl<const N: usize> PoolHandle<'_, N> {
    /// Slot index this handle publishes to and drains, doubling as
    /// the pool index for per-pool accounting (`set_pool_alive`,
    /// `record_reconnect_attempt`).
    #[must_use]
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Publish a new `WorkAssignment` into this slot's cell.
    /// Latest-wins, and unconditional: whether it reaches the ASIC is
    /// the dispatcher's decision, not the session's.
    pub fn publish_work(&self, work: WorkAssignment) {
        self.session.work[self.slot].sender().send(work);
    }

    /// Empty this slot's work cell. Sessions call it on teardown so
    /// the dispatcher never re-dispatches a dead pool's last job.
    pub fn clear_work(&self) {
        self.session.work[self.slot].sender().clear();
    }

    /// Wait for the next solution routed to this slot.
    pub async fn take_solution(&self) -> Solution {
        self.session.solutions[self.slot].receive().await
    }

    /// Try to take one solution without blocking. Returns `None` if
    /// the queue is empty.
    #[must_use]
    pub fn try_take_solution(&self) -> Option<Solution> {
        self.session.solutions[self.slot].try_receive().ok()
    }
}
