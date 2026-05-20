//! `AsicSession` — the polled-cell contract between the host and
//! whatever drives the ASIC.
//!
//! `work` is a `Watch<WorkAssignment>`: the host writes the latest
//! assignment, the ASIC polls (latest-wins). `solutions` is a
//! `Channel<Solution>`: the ASIC sends, the host drains. Both ends
//! receive a `&'static AsicSession` at construction time.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{DynReceiver, Watch};

use crate::work::{Solution, WorkAssignment};

/// Channel depth for solutions. ASIC mocks send when found; the
/// host drains in its `select!` between `client.drive()` and
/// `asic.solutions.receiver()`. Eight slots is far more headroom
/// than a CPU mock will produce between polls; an ASIC driver may
/// want to tune.
pub const SOLUTION_QUEUE_DEPTH: usize = 8;

/// `Watch` capacity: one writer (the work generator) plus one
/// reader (the ASIC). Two slots match the embassy-sync API
/// requirement.
const WATCH_RECEIVERS: usize = 2;

/// Polled host↔ASIC handoff.
///
/// `static` lifetime is the intended use: the firmware allocates
/// one in a `static` and passes `&'static AsicSession` to both the
/// host-side stratum task and the ASIC-side worker.
pub struct AsicSession {
    work: Watch<CriticalSectionRawMutex, WorkAssignment, WATCH_RECEIVERS>,
    solutions: Channel<CriticalSectionRawMutex, Solution, SOLUTION_QUEUE_DEPTH>,
}

impl AsicSession {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            work: Watch::new(),
            solutions: Channel::new(),
        }
    }

    // ---- Host side ----

    /// Publish a new `WorkAssignment`. Latest-wins: ASIC sees the
    /// most recent value on its next poll.
    pub fn publish_work(&self, work: WorkAssignment) {
        self.work.sender().send(work);
    }

    /// Try to take one solution without blocking. Returns `None` if
    /// the queue is empty.
    pub fn try_take_solution(&self) -> Option<Solution> {
        self.solutions.try_receive().ok()
    }

    /// Wait for the next solution.
    pub async fn take_solution(&self) -> Solution {
        self.solutions.receive().await
    }

    // ---- ASIC side ----

    /// Receiver handle for the work cell. Returns `None` if the
    /// `Watch`'s receiver budget is exhausted (won't happen in
    /// practice with `WATCH_RECEIVERS = 2` and one ASIC consumer).
    #[must_use]
    pub fn work_receiver(&self) -> Option<DynReceiver<'_, WorkAssignment>> {
        self.work.dyn_receiver()
    }

    /// Push a found solution into the channel. Returns `false` on
    /// full — matches the "tiny FIFO on the chip" model where the
    /// host is expected to drain promptly. Callers may log or
    /// retry; we don't surface a richer error type because the
    /// only failure mode is the queue being full.
    pub fn submit_solution(&self, solution: Solution) -> bool {
        self.solutions.try_send(solution).is_ok()
    }
}

impl Default for AsicSession {
    fn default() -> Self {
        Self::new()
    }
}
