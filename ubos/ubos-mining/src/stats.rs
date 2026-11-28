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

//! Per-chain accounting, one instance per chain slot. The table is
//! sized to [`MAX_CHAINS`], the ceiling of `ubos-machine`.
//!
//! Each `ChainStats` is owned by the board. That chain's serial actor
//! and the miner write it, and the API, the display and the metrics
//! publisher read it. The serial actor's `Reset` command also runs
//! [`ChainStats::clear`], which puts every per-bring-up field back to
//! its initial value. The chip count has one
//! writer, the miner, which stores it with the enumeration outcome that
//! produced it; the serial actor reports the count it read to the miner
//! and writes nothing here, so the count and the presence can never
//! disagree.
//!
//! The table is private to this crate, and [`ChainStatsHandle`] is how
//! the miner and the serial actors reach it. A consumer outside the crate
//! reads a [`Chains`] snapshot from `Handle::chains`.

use crate::consts::MAX_POOL_SLOTS;
use crate::hashrate::HashrateEstimator;
use portable_atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use ubos_core::index::HashboardIndex;
use ubos_core::units::{Frequency, Hashrate, Shares};

pub mod presence;

pub use presence::ChainPresence;
pub use ubos_machine::MAX_CHAINS;

#[cfg(test)]
#[path = "stats/tests/stats.rs"]
mod tests;

/// Sentinel stored in `active_pool` while the chain is mining no pool's
/// work. It lies past [`MAX_POOL_SLOTS`], so no real slot can collide.
const NO_POOL: u8 = u8::MAX;

/// Accounting for a single chain.
pub struct ChainStats {
    hashrate: HashrateEstimator,
    desync_bytes: AtomicU32,
    pool_shares: [AtomicU64; MAX_POOL_SLOTS],
    frequency_khz: AtomicU32,
    active_pool: AtomicU8,
    /// [`ChainPresence::as_code`] in the high byte and
    /// [`ChainPresence::payload`] in the low byte, so one store keeps
    /// the two consistent.
    presence: AtomicU16,
}

impl ChainStats {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hashrate: HashrateEstimator::new(),
            desync_bytes: AtomicU32::new(0),
            pool_shares: [const { AtomicU64::new(0) }; MAX_POOL_SLOTS],
            frequency_khz: AtomicU32::new(0),
            active_pool: AtomicU8::new(NO_POOL),
            presence: AtomicU16::new(0),
        }
    }

    /// Record a chip solution found against `target`, crediting the
    /// estimator and, when `pool_slot` is in range, that slot's counter,
    /// both as the solution's difficulty in [`Shares`].
    /// The counter wraps, unlike [`Self::add_desync_bytes`]: a `u64` of
    /// shares cannot wrap in practice, and a compare-and-swap would cost
    /// every solution.
    pub fn record_share(&self, target: &[u8; 32], pool_slot: usize) {
        let shares = self.hashrate.record_share(target);
        if let Some(counter) = self.pool_shares.get(pool_slot) {
            counter.fetch_add(shares.get(), Ordering::Relaxed);
        }
    }

    /// Saturating, because a mis-clocked line counts bytes at the baud
    /// rate and a wrap would read as the line recovering when it is worst.
    pub fn add_desync_bytes(&self, n: u32) {
        let _ = self
            .desync_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                Some(held.saturating_add(n))
            });
    }

    /// See [`HashrateEstimator::hashrate`].
    #[must_use]
    pub fn hashrate(&self) -> Hashrate {
        self.hashrate.hashrate()
    }

    #[must_use]
    pub fn share_count(&self) -> Shares {
        self.hashrate.share_count()
    }

    /// Zero when `slot` is out of range.
    #[must_use]
    pub fn pool_shares(&self, slot: usize) -> Shares {
        self.pool_shares
            .get(slot)
            .map_or_else(Shares::default, |counter| {
                Shares::new(counter.load(Ordering::Relaxed))
            })
    }

    #[must_use]
    pub fn desync_bytes(&self) -> u32 {
        self.desync_bytes.load(Ordering::Relaxed)
    }

    /// Chip count of a `Present` chain, zero for any other state. Read
    /// out of the presence word, so the two cannot disagree.
    #[must_use]
    pub fn chips(&self) -> u16 {
        u16::from(self.presence().chips())
    }

    pub fn set_frequency(&self, freq: Frequency) {
        self.frequency_khz.store(freq.khz(), Ordering::Relaxed);
    }

    #[must_use]
    pub fn frequency(&self) -> Frequency {
        Frequency::from_khz(self.frequency_khz.load(Ordering::Relaxed))
    }

    /// Record the pool slot whose work the chain was last given, or
    /// `None` when a work tick dispatched nothing.
    pub fn set_active_pool(&self, slot: Option<usize>) {
        let stored = slot
            .and_then(|slot| u8::try_from(slot).ok())
            .filter(|&slot| usize::from(slot) < MAX_POOL_SLOTS)
            .unwrap_or(NO_POOL);
        self.active_pool.store(stored, Ordering::Relaxed);
    }

    /// The pool slot the chain is mining, or `None` if it has no work.
    #[must_use]
    pub fn active_pool(&self) -> Option<usize> {
        let stored = self.active_pool.load(Ordering::Relaxed);
        (stored != NO_POOL).then_some(usize::from(stored))
    }

    pub fn set_presence(&self, presence: ChainPresence) {
        self.presence
            .store(presence_word(presence), Ordering::Relaxed);
    }

    #[must_use]
    pub fn presence(&self) -> ChainPresence {
        let word = self.presence.load(Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)]
        ChainPresence::from_code((word >> 8) as u8, word as u8)
    }

    /// Every field at this moment.
    #[must_use]
    pub fn snapshot(&self) -> ChainSnapshot {
        let mut pool_shares = [Shares::default(); MAX_POOL_SLOTS];
        for (slot, shares) in pool_shares.iter_mut().enumerate() {
            *shares = self.pool_shares(slot);
        }
        ChainSnapshot {
            presence: self.presence(),
            frequency: self.frequency(),
            hashrate: self.hashrate(),
            shares: self.share_count(),
            pool_shares,
            desync_bytes: self.desync_bytes(),
            active_pool: self.active_pool(),
        }
    }

    /// Reset the per-bring-up fields. The lifetime counters stay, since
    /// this runs on every chain reset.
    pub fn clear(&self) {
        self.frequency_khz.store(0, Ordering::Relaxed);
        self.active_pool.store(NO_POOL, Ordering::Relaxed);
        self.presence
            .store(presence_word(ChainPresence::Unknown), Ordering::Relaxed);
    }
}

fn presence_word(presence: ChainPresence) -> u16 {
    u16::from(presence.as_code()) << 8 | u16::from(presence.payload())
}

impl Default for ChainStats {
    fn default() -> Self {
        Self::new()
    }
}

/// The accounting of one chain at one moment, as a consumer outside the
/// miner reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainSnapshot {
    pub presence: ChainPresence,
    /// The last frequency the serial actor of the chain confirmed.
    pub frequency: Frequency,
    /// The smoothed hashrate of the chain.
    pub hashrate: Hashrate,
    /// The lifetime chip shares of the chain.
    pub shares: Shares,
    /// The lifetime chip shares that each pool slot was credited.
    pub pool_shares: [Shares; MAX_POOL_SLOTS],
    pub desync_bytes: u32,
    /// The pool slot the chain is mining, or `None` if it has no work.
    pub active_pool: Option<usize>,
}

impl ChainSnapshot {
    /// Chip count of a `Present` chain, zero for any other state.
    #[must_use]
    pub fn chips(&self) -> u16 {
        u16::from(self.presence.chips())
    }
}

/// Every wired chain at one moment, in chain order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chains(heapless::Vec<ChainSnapshot, MAX_CHAINS>);

impl Chains {
    pub fn iter(&self) -> impl Iterator<Item = &ChainSnapshot> {
        self.0.iter()
    }

    /// Saturating sum over the wired chains.
    #[must_use]
    pub fn machine_hashrate(&self) -> Hashrate {
        self.iter().fold(Hashrate::default(), |total, chain| {
            total.saturating_add(chain.hashrate)
        })
    }

    /// Saturating sum over the wired chains.
    #[must_use]
    pub fn machine_share_count(&self) -> Shares {
        self.iter().fold(Shares::default(), |total, chain| {
            total.saturating_add(chain.shares)
        })
    }

    #[must_use]
    pub fn present_chain_count(&self) -> u8 {
        let count = self
            .iter()
            .filter(|chain| chain.presence.is_present())
            .count();
        u8::try_from(count).unwrap_or(u8::MAX)
    }
}

impl FromIterator<ChainSnapshot> for Chains {
    /// Chains past [`MAX_CHAINS`] are dropped, because no machine wires
    /// more.
    fn from_iter<I: IntoIterator<Item = ChainSnapshot>>(chains: I) -> Self {
        Self(chains.into_iter().take(MAX_CHAINS).collect())
    }
}

static CHAIN_STATS: [ChainStats; MAX_CHAINS] = [const { ChainStats::new() }; MAX_CHAINS];

/// How many slots the machine wired up. Zero until the miner stores its
/// chain count, so a reader walks nothing before then.
static ACTIVE_CHAINS: AtomicU8 = AtomicU8::new(0);

/// Called once by `ubos_mining::Miner::new`, the one place that knows the
/// chain count.
pub(crate) fn set_chain_count(chains: usize) {
    ACTIVE_CHAINS.store(u8::try_from(chains).unwrap_or(u8::MAX), Ordering::Relaxed);
}

/// A board's per-chain accounting as this crate sees it. The accessors
/// hand back the [`ChainStats`] itself, not a snapshot: each reader wants
/// a different subset, and each field is one relaxed load.
#[derive(Clone, Copy)]
pub struct ChainStatsHandle {
    stats: &'static [ChainStats],
    /// How many leading slots carry a wired chain.
    active: &'static AtomicU8,
}

impl ChainStatsHandle {
    /// A handle over a caller-owned table; for tests that need a table
    /// of their own. A board uses [`handle`].
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn new(stats: &'static [ChainStats], active: &'static AtomicU8) -> Self {
        Self { stats, active }
    }

    /// Slots in the table, wired up or not.
    #[must_use]
    pub const fn slots(&self) -> usize {
        self.stats.len()
    }

    /// Wired chains; zero until the miner starts. Never larger than
    /// [`Self::slots`].
    #[must_use]
    pub fn chain_count(&self) -> usize {
        usize::from(self.active.load(Ordering::Relaxed)).min(self.slots())
    }

    /// One chain's accounting. Bounded by [`Self::slots`], not the wired
    /// prefix, because a serial task looks its slot up before the miner
    /// publishes the chain count.
    ///
    /// # Panics
    ///
    /// If `chain` is at or beyond [`Self::slots`]. An index from a
    /// request or a config field must be checked against
    /// [`Self::chain_count`] first; a panic here restarts the board.
    #[must_use]
    pub fn chain(&self, chain: HashboardIndex) -> &'static ChainStats {
        &self.stats[chain.slot()]
    }

    /// Every wired chain's accounting, in chain order.
    pub fn chains(&self) -> impl Iterator<Item = &'static ChainStats> {
        self.stats.iter().take(self.chain_count())
    }

    /// Every wired chain at this moment.
    #[must_use]
    pub fn snapshot(&self) -> Chains {
        self.chains().map(ChainStats::snapshot).collect()
    }
}

/// The board's per-chain accounting. Free because `CHAIN_STATS` is a
/// private static with one instance.
#[must_use]
pub(crate) fn handle() -> ChainStatsHandle {
    ChainStatsHandle {
        stats: &CHAIN_STATS,
        active: &ACTIVE_CHAINS,
    }
}
