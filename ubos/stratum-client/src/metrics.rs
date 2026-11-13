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

//! Per-pool counters, one [`PoolMetrics`] per slot of a [`PoolTable`].
//!
//! Every counter a pool session accumulates over its lifetime lives
//! here: accepted and rejected shares, reconnect attempts, and
//! reachability. Session tasks write through [`PoolHandle`](crate::asic_session::PoolHandle)
//! or, for the visitor's share counters, through the slot index
//! `StratumState` already carries. Readers take a [`PoolMetricsSnapshot`]
//! rather than the atomics themselves, and another crate reads through
//! [`PoolCountersHandle`].

use embassy_time::Duration;
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::run::POOL_INDEX_SPACE;

/// Period of the counter pushes from the pool session task.
pub(crate) const PUSH_PERIOD: Duration = Duration::from_secs(2);

/// One pool slot's lifetime counters.
pub struct PoolMetrics {
    shares_accepted: AtomicU64,
    shares_rejected: AtomicU64,
    reconnects: AtomicU32,
    /// Cleared by the first call to [`PoolMetrics::record_reconnect_attempt`]
    /// so the initial connection attempt after boot is not counted as a
    /// reconnect.
    first_reconnect_attempt: AtomicBool,
    alive: AtomicBool,
}

impl PoolMetrics {
    const fn new() -> Self {
        Self {
            shares_accepted: AtomicU64::new(0),
            shares_rejected: AtomicU64::new(0),
            reconnects: AtomicU32::new(0),
            first_reconnect_attempt: AtomicBool::new(true),
            alive: AtomicBool::new(false),
        }
    }

    /// Record `count` shares the pool accepted.
    pub(crate) fn record_shares_accepted(&self, count: u32) {
        self.shares_accepted
            .fetch_add(u64::from(count), Ordering::Relaxed);
    }

    /// Record one share the pool rejected.
    pub(crate) fn record_share_rejected(&self) {
        self.shares_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the reconnect counter on every call past the first.
    ///
    /// The pool session task calls this at the top of its outer
    /// reconnect loop. The very first call after boot is a no-op so a
    /// successful first connection does not register as a reconnect;
    /// every subsequent loop entry adds one, capturing both TCP-level
    /// connect failures and session-level disconnects.
    pub(crate) fn record_reconnect_attempt(&self) {
        if !self.first_reconnect_attempt.swap(false, Ordering::Relaxed) {
            self.reconnects.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Set the slot's reachability flag.
    pub(crate) fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Relaxed);
    }

    /// Accepted shares since boot.
    pub(crate) fn shares_accepted(&self) -> u64 {
        self.shares_accepted.load(Ordering::Relaxed)
    }

    /// Rejected shares since boot.
    pub(crate) fn shares_rejected(&self) -> u64 {
        self.shares_rejected.load(Ordering::Relaxed)
    }

    /// Snapshot every counter in one pass.
    pub(crate) fn snapshot(&self) -> PoolMetricsSnapshot {
        PoolMetricsSnapshot {
            shares_accepted: self.shares_accepted.load(Ordering::Relaxed),
            shares_rejected: self.shares_rejected.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            alive: self.alive.load(Ordering::Relaxed),
        }
    }
}

/// A consistent read of one slot's counters at one point in time.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PoolMetricsSnapshot {
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub reconnects: u32,
    pub alive: bool,
}

/// Every pool slot's counters, indexed by pool index. A `u8` pool
/// index fits this space by construction.
type PoolTable = [PoolMetrics; POOL_INDEX_SPACE];

/// The counters of the firmware's pool sessions.
pub(crate) static POOL_METRICS: PoolTable = [const { PoolMetrics::new() }; POOL_INDEX_SPACE];

/// The counters for `pool_index`, or `None` past [`POOL_INDEX_SPACE`]. The
/// session code of this crate counts through it.
pub(crate) fn for_slot(pool_index: usize) -> Option<&'static PoolMetrics> {
    POOL_METRICS.get(pool_index)
}

/// Records that the firmware took the pool counter handle.
static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("stratum");

/// The pool counters, which every session slot counts in and the operator
/// reports read.
///
/// The handle holds no data: its methods are the way for a reader outside
/// this crate to the private statics of this module. The session tasks of
/// this crate count in the statics directly. A firmware takes the handle
/// once and passes a copy to each reader.
#[derive(Clone, Copy)]
pub struct PoolCountersHandle(());

impl PoolCountersHandle {
    /// The firmware's one pool counter handle, over the private table of this
    /// crate. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> Self {
        TAKEN.take();
        Self(())
    }

    /// Current per-pool reachability bitmap, one bit per pool index. Set
    /// bits render as `Alive` on the dashboard; cleared bits as `Dead`.
    /// Pool reachability is a stratum concern, so it is owned here rather
    /// than in `ubos-net`. Bits of unconfigured slots stay zero and
    /// render `Dead`.
    #[must_use]
    pub fn pool_alive_bitmap(&self) -> u8 {
        let mut bitmap = 0u8;
        for (pool_index, metrics) in POOL_METRICS.iter().enumerate() {
            if metrics.snapshot().alive {
                bitmap |= 1u8 << pool_index;
            }
        }
        bitmap
    }

    /// Shares the pool has accepted since boot, summed across every pool
    /// slot.
    #[must_use]
    pub fn shares_accepted(&self) -> u64 {
        POOL_METRICS.iter().map(PoolMetrics::shares_accepted).sum()
    }

    /// Shares the pool has rejected since boot, summed across every pool
    /// slot.
    #[must_use]
    pub fn shares_rejected(&self) -> u64 {
        POOL_METRICS.iter().map(PoolMetrics::shares_rejected).sum()
    }
}
