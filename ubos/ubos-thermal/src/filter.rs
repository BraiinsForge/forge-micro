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
//! it is given over a window of the last samples. It keeps one summary
//! for each sample, so the memory of a filter does not grow with the
//! number of sensors or chips behind the source.

use ubos_core::units::Temperature;

use crate::TempStats;

#[cfg(test)]
#[path = "tests/filter.rs"]
mod tests;

/// The number of samples in the window. A reading stands for the sample
/// that took it and the two after it, so a sensor that misses two reads
/// in a row keeps its value.
const SAMPLES: usize = 3;

/// The temperatures of one sample, folded into the values that merge by
/// one operation each.
#[derive(Clone, Copy)]
struct Pane {
    max: Temperature,
    sum: i64,
    count: u32,
}

impl Pane {
    fn start(stats: TempStats) -> Self {
        Self {
            max: stats.max,
            sum: i64::from(stats.mean.millicelsius()),
            count: 1,
        }
    }

    fn merge(&mut self, other: &Self) {
        self.max = self.max.max(other.max);
        self.sum += other.sum;
        self.count += other.count;
    }
}

/// A window over the temperatures of one kind of source. It keeps a ring
/// of one pane for each sample, so one ring gives the maximum and the
/// mean without a history. The window counts samples and reads no clock,
/// so a sample that takes long costs no reading its place. A kind that is
/// silent for [`SAMPLES`] samples yields `None`.
pub(crate) struct TempFilter {
    panes: [Option<Pane>; SAMPLES],
    /// The index of the pane of the sample that is being taken.
    current: usize,
}

impl TempFilter {
    pub(crate) const fn new() -> Self {
        Self {
            panes: [None; SAMPLES],
            current: 0,
        }
    }

    /// Fold `value` into the pane of this sample.
    pub(crate) fn update(&mut self, value: Temperature) {
        self.update_stats(TempStats {
            max: value,
            mean: value,
        });
    }

    /// Fold the maximum and the mean of one sample into its pane, as one
    /// reading. A source that already holds both hands them over whole,
    /// so the filter takes one value where the source read many.
    pub(crate) fn update_stats(&mut self, stats: TempStats) {
        let pane = Pane::start(stats);
        match &mut self.panes[self.current] {
            Some(live) => live.merge(&pane),
            empty => *empty = Some(pane),
        }
    }

    /// The maximum and the mean of the window, or `None` if no
    /// temperature fell inside it.
    pub(crate) fn value(&self) -> Option<TempStats> {
        let mut live = self.panes.iter().flatten();
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

    /// End this sample and start the next one, whose pane takes the place
    /// of the oldest.
    pub(crate) fn end_sample(&mut self) {
        self.current = (self.current + 1) % SAMPLES;
        self.panes[self.current] = None;
    }
}
