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

//! The miner's view of whether a pool holds a job.

use embassy_time::{Duration, Instant, Timer};

use crate::serial::Serial;

/// How often the miner asks the actor whether a pool slot holds a job,
/// both while waiting for a pool and while watching for its loss.
pub(super) const POOL_POLL_PERIOD: Duration = Duration::from_secs(1);

/// How long every pool slot may stay empty while mining before the rail
/// is cut. Long enough for a pool to reconnect without a power cycle.
const POOL_LOSS_GRACE: Duration = Duration::from_secs(60);

/// How long no pool slot has held a job while mining.
#[derive(Default)]
pub(super) struct PoolWatch {
    /// When the slots were first seen empty; `None` while a job is held.
    empty_since: Option<Instant>,
}

impl PoolWatch {
    /// Record one observation. Returns `true` once the slots have been
    /// empty for `grace` without a job in between.
    pub(super) fn observe(&mut self, available: bool, now: Instant, grace: Duration) -> bool {
        if available {
            self.empty_since = None;
            return false;
        }
        let since = *self.empty_since.get_or_insert(now);
        now.saturating_duration_since(since) >= grace
    }

    /// Resolve once no pool slot has held a job for `POOL_LOSS_GRACE`.
    /// The future is dropped and rebuilt at every tuner step; the time
    /// already spent empty lives in `self`, so a short step cannot
    /// restart the count.
    pub(super) async fn lost(&mut self, serial: &Serial) {
        loop {
            let available = serial.work_available().await;
            if self.observe(available, Instant::now(), POOL_LOSS_GRACE) {
                return;
            }
            Timer::after(POOL_POLL_PERIOD).await;
        }
    }
}
