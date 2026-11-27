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

//! The miner's view of the temperatures that thermal publishes: the
//! dangerous temperature flag, and whether any reading is left to watch
//! the machine by.

use embassy_time::{Duration, Instant, Timer};
use ubos_thermal::{Handle, PidTemperatureSubscription};

/// How often the miner asks thermal whether a reading is left.
const UNREADABLE_POLL_PERIOD: Duration = Duration::from_secs(1);

/// How long past one sample interval of thermal no reading may be left
/// while mining before the rail is cut.
const UNREADABLE_MARGIN: Duration = Duration::from_secs(5);

/// How long no reading may be left while mining before the rail is cut,
/// when thermal samples once in each `sample_interval_ms`. Thermal
/// publishes once in each interval, so a `None` stands that long before
/// the next tick can replace it.
pub(super) fn unreadable_grace(sample_interval_ms: u32) -> Duration {
    Duration::from_millis(u64::from(sample_interval_ms)) + UNREADABLE_MARGIN
}

/// Wait for the dangerous temperature flag to be set.
pub(super) async fn danger_raised(thermal: Handle) {
    let mut flag = thermal.subscribe_dangerous();
    flag.get_and(|set| *set).await;
}

/// Wait for the dangerous temperature flag to be clear. A flag that is
/// clear already resolves at once.
pub(super) async fn danger_cleared(thermal: Handle) {
    let mut flag = thermal.subscribe_dangerous();
    flag.get_and(|set| !*set).await;
}

/// How long thermal has had no reading to publish while mining.
#[derive(Default)]
pub(super) struct TemperatureWatch {
    /// When thermal was first seen without a reading; `None` while it has
    /// one.
    silent_since: Option<Instant>,
    /// When thermal was last asked. A tuner step asks nothing, so a
    /// silence that began before a long step says nothing about the step.
    observed: Option<Instant>,
    /// Wakes on each tick of thermal, which publishes on every one.
    ticks: Option<PidTemperatureSubscription>,
    /// When a tick of thermal was last seen; `None` until the first.
    last_tick: Option<Instant>,
}

impl TemperatureWatch {
    /// Record one observation. Returns `true` once thermal has had no
    /// reading for `grace` without one in between. The count starts again
    /// after more than `grace` without an observation, because a reading
    /// can come and go unseen in that time.
    pub(super) fn observe(&mut self, silent: bool, now: Instant, grace: Duration) -> bool {
        let unwatched = self
            .observed
            .replace(now)
            .is_some_and(|last| now.saturating_duration_since(last) > grace);
        if !silent || unwatched {
            self.silent_since = None;
        }
        if !silent {
            return false;
        }
        let since = *self.silent_since.get_or_insert(now);
        now.saturating_duration_since(since) >= grace
    }

    /// Record whether a tick of thermal came since the last look, and
    /// return whether the last one is older than `grace`. A thermal task
    /// that never ticks is never stalled.
    fn stalled(&mut self, thermal: Handle, now: Instant, grace: Duration) -> bool {
        let ticked = self
            .ticks
            .get_or_insert_with(|| thermal.subscribe_pid_temperature())
            .try_changed()
            .is_some();
        if ticked {
            self.last_tick = Some(now);
        }
        self.last_tick
            .is_some_and(|last| now.saturating_duration_since(last) > grace)
    }

    /// Resolve once thermal has had no reading for `grace`, or has stopped
    /// ticking for that long.
    /// The future is dropped and rebuilt at every tuner step; the time
    /// already spent silent lives in `self`, so a short step cannot restart
    /// the count. A thermal task that never publishes keeps this pending,
    /// so a deployment with no thermal task never trips it.
    pub(super) async fn unreadable(&mut self, thermal: Handle, grace: Duration) {
        loop {
            let now = Instant::now();
            let stalled = self.stalled(thermal, now, grace);
            if self.observe(thermal.has_no_reading(), now, grace) || stalled {
                return;
            }
            Timer::after(UNREADABLE_POLL_PERIOD).await;
        }
    }
}
