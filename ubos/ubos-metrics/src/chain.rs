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

//! Per-chain accounting, one instance per chain slot, and [`MAX_CHAINS`],
//! the ceiling it is sized to. This crate sits below every consumer of
//! the table, which is why both live here.
//!
//! [`MAX_CHAINS`] lives here because this table is what it sizes, and
//! this crate sits below every consumer of the table: `ubos-mining`
//! writes it, `ubos-thermal`, `ubos-display` and `ubos-api` read it, and
//! `ubos-machine` re-exports the ceiling for the descriptors that assert
//! against it.
//!
//!
//! Each `ChainStats` is owned by the board. That chain's serial actor
//! and the miner write it, the thermal loop is the one producer of the
//! [`ChainStats::hottest`] and [`ChainStats::voltage`] readings, and the
//! API, the display and the metrics publisher read it. The serial actor's
//! `Reset` command also runs [`ChainStats::clear`], which puts every
//! per-bring-up field back to its initial value, `hottest` included, so
//! a reading never outlives the bring-up that produced it. `voltage`
//! is the exception: the thermal loop stores `None` for a silent chain,
//! so it stays the only writer of that field. The chip count has one
//! writer, the miner, which stores it with the enumeration outcome that
//! produced it; the serial actor reports the count it read to the miner
//! and writes nothing here, so the count and the presence can never
//! disagree.
//!
//! Everything outside this crate reaches the table through
//! [`ChainStatsHandle`], which [`handle`] returns. The static itself is
//! private, so no consumer holds a second door to it.

use crate::hashrate::HashrateEstimator;
use portable_atomic::{AtomicI32, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use ubos_core::index::HashboardIndex;
use ubos_core::units::{Frequency, Temperature, Voltage};

pub mod presence;

pub use presence::ChainPresence;

#[cfg(test)]
#[path = "chain/tests/stats.rs"]
mod tests;

/// The most chains one machine drives. The library crates cannot see a
/// board's descriptor, so their per-chain tables are sized to this, and
/// every machine descriptor asserts its chain count fits under it.
pub const MAX_CHAINS: usize = 3;

/// Pool slots a chain's share accounting and the scheduler weights span.
pub const MAX_POOL_SLOTS: usize = 8;

/// Sentinel in `hottest_mc` before any reading lands; no sensor reports
/// it.
const NO_TEMPERATURE: i32 = i32::MIN;

/// Sentinel stored in `voltage_mv` when the chain has no complete
/// voltage sum. It is far above any rail voltage in millivolts.
const NO_VOLTAGE: u32 = u32::MAX;

/// Sentinel stored in `active_pool` while the chain is mining no pool's
/// work. It lies past [`MAX_POOL_SLOTS`], so no real slot can collide.
const NO_POOL: u8 = u8::MAX;

/// Accounting for a single chain.
pub struct ChainStats {
    hashrate: HashrateEstimator,
    desync_bytes: AtomicU32,
    pool_shares: [AtomicU64; MAX_POOL_SLOTS],
    frequency_khz: AtomicU32,
    hottest_mc: AtomicI32,
    voltage_mv: AtomicU32,
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
            hottest_mc: AtomicI32::new(NO_TEMPERATURE),
            voltage_mv: AtomicU32::new(NO_VOLTAGE),
            active_pool: AtomicU8::new(NO_POOL),
            presence: AtomicU16::new(0),
        }
    }

    /// Record a chip solution found against `target`, crediting the
    /// estimator and, when `pool_slot` is in range, that slot's counter,
    /// both as the solution's difficulty in shares of
    /// [`HASHES_PER_SHARE`](crate::hashrate::HASHES_PER_SHARE) hashes.
    /// The counter wraps, unlike [`Self::add_desync_bytes`]: a `u64` of
    /// shares cannot wrap in practice, and a compare-and-swap would cost
    /// every solution.
    pub fn record_share(&self, target: &[u8; 32], pool_slot: usize) {
        let shares = self.hashrate.record_share(target);
        if let Some(counter) = self.pool_shares.get(pool_slot) {
            counter.fetch_add(shares, Ordering::Relaxed);
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
    pub fn hashrate(&self) -> u64 {
        self.hashrate.hashrate()
    }

    #[must_use]
    pub fn share_count(&self) -> u64 {
        self.hashrate.share_count()
    }

    /// Zero when `slot` is out of range.
    #[must_use]
    pub fn pool_shares(&self, slot: usize) -> u64 {
        self.pool_shares
            .get(slot)
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
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

    /// `None` clears the reading.
    pub fn set_hottest(&self, value: Option<Temperature>) {
        self.hottest_mc.store(
            value.map_or(NO_TEMPERATURE, Temperature::millicelsius),
            Ordering::Relaxed,
        );
    }

    /// `None` until a reading lands.
    #[must_use]
    pub fn hottest(&self) -> Option<Temperature> {
        let stored = self.hottest_mc.load(Ordering::Relaxed);
        (stored != NO_TEMPERATURE).then(|| Temperature::from_millicelsius(stored))
    }

    /// Record the sum of the core voltages that the chips of this chain
    /// report, or clear it with `None`.
    pub fn set_voltage(&self, value: Option<Voltage>) {
        self.voltage_mv
            .store(value.map_or(NO_VOLTAGE, Voltage::mv), Ordering::Relaxed);
    }

    /// The sum of the core voltages that the chips of this chain report,
    /// or `None` if the chain has no complete sum.
    #[must_use]
    pub fn voltage(&self) -> Option<Voltage> {
        let stored = self.voltage_mv.load(Ordering::Relaxed);
        (stored != NO_VOLTAGE).then(|| Voltage::from_mv(stored))
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

    /// Reset the per-bring-up fields. The lifetime counters stay, since
    /// this runs on every chain reset.
    pub fn clear(&self) {
        self.frequency_khz.store(0, Ordering::Relaxed);
        self.hottest_mc.store(NO_TEMPERATURE, Ordering::Relaxed);
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

static CHAIN_STATS: [ChainStats; MAX_CHAINS] = [const { ChainStats::new() }; MAX_CHAINS];

/// How many slots the machine wired up. Zero until the miner stores its
/// chain count, so a reader walks nothing before then.
static ACTIVE_CHAINS: AtomicU8 = AtomicU8::new(0);

/// Called once by `ubos_mining::Miner::new`, the one place that knows the
/// chain count.
pub fn set_chain_count(chains: usize) {
    ACTIVE_CHAINS.store(u8::try_from(chains).unwrap_or(u8::MAX), Ordering::Relaxed);
}

/// A board's per-chain accounting as consumers see it. The accessors
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
    #[must_use]
    pub const fn new(stats: &'static [ChainStats], active: &'static AtomicU8) -> Self {
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

    /// The lowest voltage sum that a wired chain reports, or `None` when
    /// no chain has a complete sum. The chains of one board share a rail,
    /// so the lowest sum is the reading that shows the droop of the board,
    /// and a higher sum of another chain would hide it.
    #[must_use]
    pub fn lowest_voltage(&self) -> Option<Voltage> {
        self.chains().filter_map(ChainStats::voltage).min()
    }

    /// Saturating sum over the wired chains.
    #[must_use]
    pub fn machine_hashrate(&self) -> u64 {
        self.chains()
            .fold(0u64, |total, chain| total.saturating_add(chain.hashrate()))
    }

    /// Saturating sum over the wired chains.
    #[must_use]
    pub fn machine_share_count(&self) -> u64 {
        self.chains().fold(0u64, |total, chain| {
            total.saturating_add(chain.share_count())
        })
    }

    #[must_use]
    pub fn present_chain_count(&self) -> u8 {
        let count = self
            .chains()
            .filter(|chain| chain.presence().is_present())
            .count();
        u8::try_from(count).unwrap_or(u8::MAX)
    }
}

/// The board's per-chain accounting. Free because `CHAIN_STATS` is a
/// private static with one instance.
#[must_use]
pub fn handle() -> ChainStatsHandle {
    ChainStatsHandle {
        stats: &CHAIN_STATS,
        active: &ACTIVE_CHAINS,
    }
}
