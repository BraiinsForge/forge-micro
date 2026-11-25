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

//! Per-pool counters, one [`PoolMetrics`] per pool slot.
//!
//! Every counter a pool session accumulates over its lifetime lives
//! here: accepted and rejected shares, their work, the pool difficulty,
//! reconnect attempts, and reachability. Session tasks write through
//! [`PoolHandle`](crate::asic_session::PoolHandle) or, for the visitor's
//! share counters, through the slot index `StratumState` already
//! carries. Readers take a [`PoolMetricsSnapshot`] rather than the
//! atomics themselves, and another crate reads through
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
    /// Sum of the `work` of the accepted submits, in diff-1 share units.
    work_accepted: AtomicU64,
    /// Sum of the `work` of the rejected submits, in diff-1 share units.
    work_rejected: AtomicU64,
    /// Difficulty of the target the pool last sent, `0` before the first
    /// and for a target outside the approximation.
    difficulty: AtomicU64,
    reconnects: AtomicU32,
    /// Cleared by the first call to [`PoolMetrics::record_reconnect_attempt`];
    /// see its doc for why.
    first_reconnect_attempt: AtomicBool,
    alive: AtomicBool,
}

impl PoolMetrics {
    const fn new() -> Self {
        Self {
            shares_accepted: AtomicU64::new(0),
            shares_rejected: AtomicU64::new(0),
            work_accepted: AtomicU64::new(0),
            work_rejected: AtomicU64::new(0),
            difficulty: AtomicU64::new(0),
            reconnects: AtomicU32::new(0),
            first_reconnect_attempt: AtomicBool::new(true),
            alive: AtomicBool::new(false),
        }
    }

    /// Record `count` shares the pool accepted, worth `work` in all.
    pub(crate) fn record_shares_accepted(&self, count: u32, work: u64) {
        self.shares_accepted
            .fetch_add(u64::from(count), Ordering::Relaxed);
        self.work_accepted.fetch_add(work, Ordering::Relaxed);
    }

    /// Record one share the pool rejected, worth `work`.
    pub(crate) fn record_share_rejected(&self, work: u64) {
        self.shares_rejected.fetch_add(1, Ordering::Relaxed);
        self.work_rejected.fetch_add(work, Ordering::Relaxed);
    }

    /// Record the difficulty of the target the pool sent, or `0` for a
    /// slot that no session backs.
    pub(crate) fn set_difficulty(&self, difficulty: u64) {
        self.difficulty.store(difficulty, Ordering::Relaxed);
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
            work_accepted: self.work_accepted.load(Ordering::Relaxed),
            work_rejected: self.work_rejected.load(Ordering::Relaxed),
            difficulty: self.difficulty.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            alive: self.alive.load(Ordering::Relaxed),
        }
    }
}

/// A consistent read of one slot's counters at one point in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolMetricsSnapshot {
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub work_accepted: u64,
    pub work_rejected: u64,
    pub difficulty: u64,
    pub reconnects: u32,
    pub alive: bool,
}

/// Every pool slot's counters, indexed by pool index; see
/// [`crate::run::POOL_INDEX_SPACE`] for the size bound.
type PoolTable = [PoolMetrics; POOL_INDEX_SPACE];

pub(crate) static POOL_METRICS: PoolTable = [const { PoolMetrics::new() }; POOL_INDEX_SPACE];

/// The counters for `pool_index`, or `None` past [`POOL_INDEX_SPACE`]. The
/// session code of this crate counts through it.
pub(crate) fn for_slot(pool_index: usize) -> Option<&'static PoolMetrics> {
    POOL_METRICS.get(pool_index)
}

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("stratum");

/// Reader-side handle to the pool counters. It holds no data, because the
/// session tasks of this crate count in the private statics directly, and a
/// firmware takes it once and passes a copy to each reader.
#[derive(Clone, Copy)]
pub struct PoolCountersHandle(());

impl PoolCountersHandle {
    /// The firmware's one pool counter handle. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> Self {
        TAKEN.take();
        Self(())
    }

    /// Per-pool reachability bitmap, one bit per pool index: a set bit renders
    /// as `Alive` on the dashboard, a clear bit as `Dead`, including an
    /// unconfigured slot. Owned here rather than in `ubos-net` because
    /// reachability is a stratum concern.
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
