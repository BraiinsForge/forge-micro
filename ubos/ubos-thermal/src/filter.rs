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

//! The window filter of one kind of temperature source.
//!
//! A [`TempFilter`] yields the maximum and the mean of the temperatures
//! it is given over a sliding window. It cuts the window into
//! equal panes and keeps one summary for each, so the memory of a filter
//! does not grow with the number of sensors or chips behind the source.

use embassy_time::{Duration, Instant};
use ubos_core::units::Temperature;

use crate::TempStats;

#[cfg(test)]
#[path = "tests/filter.rs"]
mod tests;

/// The number of panes that one sample interval spans. Two panes keep a
/// reading in the pane of its own tick while a tick that falls late still
/// lands in the next one.
const PANES_PER_INTERVAL: u32 = 2;

/// The number of panes in the ring. The window is between
/// `PANES - 1` and `PANES` pane widths long. A pane is half a sample
/// interval, see [`PANES_PER_INTERVAL`], so the reading before one missed
/// poll is four panes old at the next tick, and five when that tick falls
/// late across a pane boundary. A sixth pane keeps it inside the window in
/// both cases.
const PANES: usize = 6;

/// The temperatures of one pane width, folded into the values that merge
/// by one operation each.
#[derive(Clone, Copy)]
struct Pane {
    max: Temperature,
    sum: i64,
    count: u32,
    epoch: u64,
}

impl Pane {
    fn start(stats: TempStats, epoch: u64) -> Self {
        Self {
            max: stats.max,
            sum: i64::from(stats.mean.millicelsius()),
            count: 1,
            epoch,
        }
    }

    fn merge(&mut self, other: &Self) {
        self.max = self.max.max(other.max);
        self.sum += other.sum;
        self.count += other.count;
    }
}

/// A sliding window over the temperatures of one kind of source. It keeps
/// a ring of panes, so one ring gives the maximum and the mean without a
/// history. A kind that is silent for the length of the window
/// yields `None` and needs no timer. The owner of the filter chooses which
/// readings it takes.
pub(crate) struct TempFilter {
    width: Duration,
    panes: [Option<Pane>; PANES],
}

impl TempFilter {
    /// A filter for readings that arrive once in each `interval`. The
    /// shortest window is `PANES - 1` pane widths, and it must hold the
    /// last reading of every sensor of the kind.
    pub(crate) const fn for_interval(interval: Duration) -> Self {
        Self {
            width: Duration::from_ticks(interval.as_ticks() / PANES_PER_INTERVAL as u64),
            panes: [None; PANES],
        }
    }

    /// The pane number of `now`, which counts widths since the clock began.
    /// An interval below two ticks gives a width of zero, which counts as
    /// one tick.
    fn epoch(&self, now: Instant) -> u64 {
        now.as_ticks() / self.width.as_ticks().max(1)
    }

    /// Fold `value` into the pane of `now`.
    pub(crate) fn update(&mut self, value: Temperature, now: Instant) {
        let stats = TempStats {
            max: value,
            mean: value,
        };
        self.update_stats(stats, now);
    }

    /// Fold the maximum and the mean of one tick into the pane of `now`,
    /// as one reading. A source that already holds both for the tick
    /// hands them over whole, so the filter takes one value where the
    /// source read many.
    pub(crate) fn update_stats(&mut self, stats: TempStats, now: Instant) {
        let epoch = self.epoch(now);
        let pane = Pane::start(stats, epoch);
        // The modulo keeps the index below `PANES`.
        #[allow(clippy::cast_possible_truncation)]
        let slot = &mut self.panes[(epoch % PANES as u64) as usize];
        match slot {
            Some(live) if live.epoch == epoch => live.merge(&pane),
            _ => *slot = Some(pane),
        }
    }

    /// The maximum and the mean of the panes of the last `PANES` widths,
    /// or `None` if no temperature fell inside them.
    pub(crate) fn value(&self, now: Instant) -> Option<TempStats> {
        let epoch = self.epoch(now);
        let mut live = self
            .panes
            .iter()
            .flatten()
            .filter(|pane| pane.epoch <= epoch && epoch - pane.epoch < PANES as u64);
        let mut total = *live.next()?;
        for pane in live {
            total.merge(pane);
        }
        Some(TempStats {
            max: total.max,
            // The mean of i32 values is an i32.
            #[allow(clippy::cast_possible_truncation)]
            mean: Temperature::from_millicelsius((total.sum / i64::from(total.count)) as i32),
        })
    }
}
