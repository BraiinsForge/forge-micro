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

//! Per-chain accounting, one instance per chain slot, and the ceiling
//! on how many chains a machine drives.
//!
//! [`MAX_CHAINS`] lives here because this table is what it sizes, and
//! this crate sits below every consumer of the table: `ubos-mining`
//! writes it, `ubos-thermal`, `ubos-display` and `ubos-api` read it, and
//! `ubos-machine` re-exports the ceiling for the descriptors that assert
//! against it.
//!
//!
//! Each `ChainStats` is owned by the board. That chain's serial actor
//! and the miner write it, the thermal loop is the one producer of
//! [`ChainStats::hottest`] readings, and the API, the display and
//! the metrics publisher read it. The serial actor's `Reset` command
//! also runs [`ChainStats::clear`], which puts every per-bring-up field
//! back to its initial value, `hottest` included, so a reading
//! never outlives the bring-up that produced it. The chip count has one
//! writer, the miner, which stores it with the enumeration outcome that
//! produced it; the serial actor reports the count it read to the miner
//! and writes nothing here, so the count and the presence can never
//! disagree.
//!
//! Everything outside this crate reaches the table through
//! [`ChainStatsHandle`], which [`handle`] returns. The static itself is
//! private, so no consumer holds a second door to it.
//!
//! [`CHAIN_STATS`] carries no `ccmram-bss` link section, unlike the
//! mining mailbox statics in `ubos_mining::control`. A static in that section
//! is zeroed before main and then handed out as fully initialised, so
//! `ubos-core`'s contract is that its all-zero image must equal what
//! `new()` builds. [`ChainStats::new`] starts the hottest die at
//! `NO_TEMPERATURE`, which is `i32::MIN`, so the table fails that
//! contract: a relocated table would come up reporting zero
//! millidegrees on every chain instead of no reading at all. Do not add
//! the attribute.

use crate::hashrate::HashrateEstimator;
use portable_atomic::{AtomicI32, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use ubos_core::index::HashboardIndex;
use ubos_core::units::{Frequency, Temperature};

pub mod presence;

pub use presence::ChainPresence;

#[cfg(test)]
#[path = "chain/tests/stats.rs"]
mod tests;

/// The most chains one machine drives. The per-chain tables the library
/// crates own cannot see a board's descriptor, so they are sized to this
/// ceiling: [`CHAIN_STATS`] here, the hashboard serial vector in
/// `ubos-api`, and the work receiver budget `ubos-firmware` sizes into
/// the stratum session. Every machine descriptor in `ubos-machine`
/// asserts its chain count fits under it at compile time. A machine with
/// more chains raises it here and nowhere else.
pub const MAX_CHAINS: usize = 3;

/// Number of pool slots a chain's work split and share accounting
/// track: the fixed width of the `pool_shares` array in [`ChainStats`]
/// and of the weights `ubos-mining` hands its scheduler.
pub const MAX_POOL_SLOTS: usize = 8;

/// Sentinel stored in `hottest_mc` when no temperature reading has
/// landed yet. `i32::MIN` is outside any temperature a real sensor
/// reports, so it cannot collide with a genuine reading.
const NO_TEMPERATURE: i32 = i32::MIN;

/// Sentinel stored in `active_pool` while the chain is mining no pool's
/// work. It lies past [`MAX_POOL_SLOTS`], so no real slot can collide.
const NO_POOL: u8 = u8::MAX;

/// Accounting for a single chain: the lifetime hashrate estimate and
/// share counters, plus the chain's last-known chip count, frequency,
/// hottest die temperature, and the pool slot it is mining.
pub struct ChainStats {
    hashrate: HashrateEstimator,
    desync_bytes: AtomicU32,
    pool_shares: [AtomicU64; MAX_POOL_SLOTS],
    frequency_khz: AtomicU32,
    hottest_mc: AtomicI32,
    active_pool: AtomicU8,
    /// [`ChainPresence::as_code`] in the high byte and
    /// [`ChainPresence::payload`] in the low byte, so one store keeps
    /// the two consistent.
    presence: AtomicU16,
}

impl ChainStats {
    /// Construct a `ChainStats` with every counter at zero and no
    /// temperature reading yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hashrate: HashrateEstimator::new(),
            desync_bytes: AtomicU32::new(0),
            pool_shares: [const { AtomicU64::new(0) }; MAX_POOL_SLOTS],
            frequency_khz: AtomicU32::new(0),
            hottest_mc: AtomicI32::new(NO_TEMPERATURE),
            active_pool: AtomicU8::new(NO_POOL),
            presence: AtomicU16::new(0),
        }
    }

    /// Record a chip solution found on this chain against `target`,
    /// crediting the hashrate estimator and, when `pool_slot` is in
    /// range, that pool slot's share counter. Both count the solution
    /// as its difficulty in shares of [`HASHES_PER_SHARE`](crate::hashrate::HASHES_PER_SHARE) hashes, so
    /// a chip filtering above difficulty one still reports the work
    /// it did.
    ///
    /// The share counter wraps rather than saturating, unlike
    /// [`Self::add_desync_bytes`]. It adds the solution's difficulty
    /// into a `u64`, which no chain fills in its lifetime, and a
    /// compare-and-swap loop on this path would cost every solution
    /// for a case that cannot arrive.
    pub fn record_share(&self, target: &[u8; 32], pool_slot: usize) {
        let shares = self.hashrate.record_share(target);
        if let Some(counter) = self.pool_shares.get(pool_slot) {
            counter.fetch_add(shares, Ordering::Relaxed);
        }
    }

    /// Add `n` bytes to the chain's lifetime desync byte count,
    /// saturating at `u32::MAX`.
    ///
    /// Saturating rather than wrapping, because this one is reachable:
    /// the counter takes bytes rather than events, and a chain whose
    /// line is mis-clocked counts them at the baud rate. A wrap would
    /// show as the count falling, which reads as the line recovering at
    /// the moment it is worst.
    pub fn add_desync_bytes(&self, n: u32) {
        let _ = self
            .desync_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                Some(held.saturating_add(n))
            });
    }

    /// Smoothed hashrate in hashes per second. See
    /// [`HashrateEstimator::hashrate`].
    #[must_use]
    pub fn hashrate(&self) -> u64 {
        self.hashrate.hashrate()
    }

    /// Total shares, in units of [`HASHES_PER_SHARE`](crate::hashrate::HASHES_PER_SHARE), this chain has
    /// found since it was created.
    #[must_use]
    pub fn share_count(&self) -> u64 {
        self.hashrate.share_count()
    }

    /// Shares, in units of [`HASHES_PER_SHARE`](crate::hashrate::HASHES_PER_SHARE), this chain has found
    /// for the given pool slot, or 0 if `slot` is out of range.
    #[must_use]
    pub fn pool_shares(&self, slot: usize) -> u64 {
        self.pool_shares
            .get(slot)
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
    }

    /// Lifetime count of desync bytes seen on this chain's serial
    /// line.
    #[must_use]
    pub fn desync_bytes(&self) -> u32 {
        self.desync_bytes.load(Ordering::Relaxed)
    }

    /// Chip count found on this chain's most recent bring-up: the
    /// payload of a `Present` presence, zero for any other state.
    ///
    /// Read out of the presence word rather than kept in a field of its
    /// own, so the count and the presence cannot disagree. Widened from
    /// the `u8` the chain enumerates in because every reader counts
    /// chips across chains or formats the total.
    #[must_use]
    pub fn chips(&self) -> u16 {
        u16::from(self.presence().chips())
    }

    /// Record the chain's current operating frequency.
    pub fn set_frequency(&self, freq: Frequency) {
        self.frequency_khz.store(freq.khz(), Ordering::Relaxed);
    }

    /// The chain's current operating frequency.
    #[must_use]
    pub fn frequency(&self) -> Frequency {
        Frequency::from_khz(self.frequency_khz.load(Ordering::Relaxed))
    }

    /// Record the hottest die temperature this chain reported, or clear
    /// the reading with `None`.
    pub fn set_hottest(&self, value: Option<Temperature>) {
        self.hottest_mc.store(
            value.map_or(NO_TEMPERATURE, Temperature::millicelsius),
            Ordering::Relaxed,
        );
    }

    /// The hottest die temperature this chain has reported, or `None` if no
    /// reading has landed yet.
    #[must_use]
    pub fn hottest(&self) -> Option<Temperature> {
        let stored = self.hottest_mc.load(Ordering::Relaxed);
        (stored != NO_TEMPERATURE).then(|| Temperature::from_millicelsius(stored))
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

    /// Record the chain's enumeration outcome.
    pub fn set_presence(&self, presence: ChainPresence) {
        self.presence
            .store(presence_word(presence), Ordering::Relaxed);
    }

    /// The chain's most recently recorded enumeration outcome.
    #[must_use]
    pub fn presence(&self) -> ChainPresence {
        let word = self.presence.load(Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)]
        ChainPresence::from_code((word >> 8) as u8, word as u8)
    }

    /// Reset the per-bring-up fields: frequency, hottest temperature,
    /// active pool, and presence, which carries the chip count. Lifetime counters,
    /// meaning hashrate, share count, pool shares, and desync bytes, are
    /// not touched, because `clear` runs on a chain reset and those
    /// counters track the chain's whole life rather than its current
    /// bring-up.
    pub fn clear(&self) {
        self.frequency_khz.store(0, Ordering::Relaxed);
        self.hottest_mc.store(NO_TEMPERATURE, Ordering::Relaxed);
        self.active_pool.store(NO_POOL, Ordering::Relaxed);
        self.presence
            .store(presence_word(ChainPresence::Unknown), Ordering::Relaxed);
    }
}

/// Pack a presence into the word `ChainStats::presence` stores.
fn presence_word(presence: ChainPresence) -> u16 {
    u16::from(presence.as_code()) << 8 | u16::from(presence.payload())
}

impl Default for ChainStats {
    fn default() -> Self {
        Self::new()
    }
}

/// One `ChainStats` per chain slot a board can own.
static CHAIN_STATS: [ChainStats; MAX_CHAINS] = [const { ChainStats::new() }; MAX_CHAINS];

/// How many of [`CHAIN_STATS`]'s slots the machine actually wired up.
///
/// Zero until `ubos_mining::Miner::new` stores the board's chain count, so
/// a reader that walks this many slots reports nothing before the miner
/// starts rather than reporting the unused slots of the table.
static ACTIVE_CHAINS: AtomicU8 = AtomicU8::new(0);

/// Record how many chains the machine wired up. Called once by
/// `ubos_mining::Miner::new`, which is the only place that knows the
/// board's chain count.
pub fn set_chain_count(chains: usize) {
    ACTIVE_CHAINS.store(u8::try_from(chains).unwrap_or(u8::MAX), Ordering::Relaxed);
}

/// A board's per-chain accounting, as every consumer outside this crate
/// sees it.
///
/// A `Copy` value over the table, so the API, the display, the metrics
/// publisher and the thermal loop hold a handle rather than reach for
/// this module's static.
///
/// The per-chain accessors hand back the [`ChainStats`] itself rather
/// than a snapshot of it. Each reader wants a different subset of the
/// fields and each field is one relaxed load, so a snapshot type would
/// copy more than any caller reads and would still be stale by the time
/// it was formatted.
#[derive(Clone, Copy)]
pub struct ChainStatsHandle {
    stats: &'static [ChainStats],
    /// How many leading slots carry a wired chain. The board's table reads
    /// [`ACTIVE_CHAINS`]; a caller-owned table reads the counter the
    /// caller gave it.
    active: &'static AtomicU8,
}

impl ChainStatsHandle {
    /// Build a handle over a table the caller owns, with `active` as
    /// the count of its leading slots that carry a chain.
    ///
    /// [`handle`] is what a board uses. This is for a test that needs a
    /// table of its own, so one test's counters cannot be read by the
    /// next.
    #[must_use]
    pub const fn new(stats: &'static [ChainStats], active: &'static AtomicU8) -> Self {
        Self { stats, active }
    }

    /// How many chain slots the table holds, wired up or not.
    #[must_use]
    pub const fn slots(&self) -> usize {
        self.stats.len()
    }

    /// How many chains the machine wired up, and so how many leading
    /// slots of the table carry a real chain.
    ///
    /// For the board's table, zero until the miner starts: the chain
    /// count reaches this module when `ubos_mining::Miner::new` runs, so a
    /// reader that walks this many slots walks none before that. Never
    /// larger than [`Self::slots`].
    #[must_use]
    pub fn chain_count(&self) -> usize {
        usize::from(self.active.load(Ordering::Relaxed)).min(self.slots())
    }

    /// One chain's accounting.
    ///
    /// Indexes the whole table, not the wired prefix: a serial task
    /// looks its own slot up before `ubos_mining::Miner::new` has published
    /// the chain count, so the bound here is [`Self::slots`].
    ///
    /// # Panics
    ///
    /// Panics if `chain` is at or beyond [`Self::slots`]. Every caller
    /// today passes an index a board constant bounds, which
    /// `MachineDescriptor`'s own `chains <= MAX_CHAINS` assertion holds
    /// under the table's length at compile time. An index that comes
    /// from a request path or a config field has no such bound and must
    /// be checked against [`Self::chain_count`] before it reaches here,
    /// since a panic on this target restarts the board.
    #[must_use]
    pub fn chain(&self, chain: HashboardIndex) -> &'static ChainStats {
        &self.stats[chain.slot()]
    }

    /// Every wired chain's accounting, in chain order: the first
    /// [`Self::chain_count`] slots of the table, so no reader ever
    /// walks a slot the board did not wire up.
    pub fn chains(&self) -> impl Iterator<Item = &'static ChainStats> {
        self.stats.iter().take(self.chain_count())
    }

    /// Sum of every chain's smoothed hashrate, saturating at `u64::MAX`.
    #[must_use]
    pub fn machine_hashrate(&self) -> u64 {
        self.chains()
            .fold(0u64, |total, chain| total.saturating_add(chain.hashrate()))
    }

    /// Sum of every chain's lifetime share count, saturating at
    /// `u64::MAX`.
    #[must_use]
    pub fn machine_share_count(&self) -> u64 {
        self.chains().fold(0u64, |total, chain| {
            total.saturating_add(chain.share_count())
        })
    }

    /// Count of chains whose most recently recorded presence is
    /// `Present`.
    #[must_use]
    pub fn present_chain_count(&self) -> u8 {
        let count = self
            .chains()
            .filter(|chain| chain.presence().is_present())
            .count();
        u8::try_from(count).unwrap_or(u8::MAX)
    }
}

/// The board's per-chain accounting.
///
/// A free function because [`CHAIN_STATS`] is a private static with one
/// instance for the whole board, so there is no receiver to hang it on.
#[must_use]
pub fn handle() -> ChainStatsHandle {
    ChainStatsHandle {
        stats: &CHAIN_STATS,
        active: &ACTIVE_CHAINS,
    }
}
