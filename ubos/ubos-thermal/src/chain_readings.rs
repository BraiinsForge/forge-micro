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

//! The hottest die and the voltage sum of each chain, as the chain
//! sensor last read them.
//!
//! [`crate::ChainTemperature`] is the one writer. The API and the tuner
//! read them through [`crate::Handle::chains`].

use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use ubos_core::index::HashboardIndex;
use ubos_core::units::{Temperature, Voltage};
use ubos_machine::MAX_CHAINS;

/// Sentinel in `hottest_mc` before any reading lands; no sensor reports
/// it.
const NO_TEMPERATURE: i32 = i32::MIN;

/// Sentinel stored in `voltage_mv` when the chain has no complete
/// voltage sum. It is far above any rail voltage in millivolts.
const NO_VOLTAGE: u32 = u32::MAX;

/// The readings of one chain.
pub(crate) struct ChainReading {
    hottest_mc: AtomicI32,
    voltage_mv: AtomicU32,
}

impl ChainReading {
    pub(crate) const fn new() -> Self {
        Self {
            hottest_mc: AtomicI32::new(NO_TEMPERATURE),
            voltage_mv: AtomicU32::new(NO_VOLTAGE),
        }
    }
}

static CHAIN_READINGS: [ChainReading; MAX_CHAINS] = [const { ChainReading::new() }; MAX_CHAINS];

/// The readings of every chain slot as a consumer sees them.
#[derive(Clone, Copy)]
pub struct ChainReadings {
    table: &'static [ChainReading],
}

impl ChainReadings {
    /// The readings over a caller-owned table, for a test that needs a
    /// table of its own.
    #[cfg(test)]
    pub(crate) const fn new(table: &'static [ChainReading]) -> Self {
        Self { table }
    }

    /// The readings of the board.
    pub(crate) const fn board() -> Self {
        Self {
            table: &CHAIN_READINGS,
        }
    }

    /// Slots in the table, wired up or not.
    #[must_use]
    pub const fn slots(&self) -> usize {
        self.table.len()
    }

    /// The hottest die of `chain`, or `None` until a reading lands and
    /// for a slot past the table.
    #[must_use]
    pub fn hottest(&self, chain: HashboardIndex) -> Option<Temperature> {
        let stored = self
            .table
            .get(chain.slot())?
            .hottest_mc
            .load(Ordering::Relaxed);
        (stored != NO_TEMPERATURE).then(|| Temperature::from_millicelsius(stored))
    }

    /// The sum of the core voltages that the chips of `chain` report, or
    /// `None` if the chain has no complete sum.
    #[must_use]
    pub fn voltage(&self, chain: HashboardIndex) -> Option<Voltage> {
        let stored = self
            .table
            .get(chain.slot())?
            .voltage_mv
            .load(Ordering::Relaxed);
        (stored != NO_VOLTAGE).then(|| Voltage::from_mv(stored))
    }

    /// The lowest voltage sum that a chain reports, or `None` when no
    /// chain has a complete sum. The chains of one board share a rail,
    /// so the lowest sum is the reading that shows the droop of the board,
    /// and a higher sum of another chain would hide it.
    #[must_use]
    pub fn lowest_voltage(&self) -> Option<Voltage> {
        HashboardIndex::range(self.slots())
            .filter_map(|chain| self.voltage(chain))
            .min()
    }

    /// `None` clears the reading.
    pub(crate) fn set_hottest(&self, chain: HashboardIndex, value: Option<Temperature>) {
        self.table[chain.slot()].hottest_mc.store(
            value.map_or(NO_TEMPERATURE, Temperature::millicelsius),
            Ordering::Relaxed,
        );
    }

    /// `None` clears the reading.
    pub(crate) fn set_voltage(&self, chain: HashboardIndex, value: Option<Voltage>) {
        self.table[chain.slot()]
            .voltage_mv
            .store(value.map_or(NO_VOLTAGE, Voltage::mv), Ordering::Relaxed);
    }
}
