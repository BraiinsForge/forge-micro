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

//! The temperatures that thermal publishes.
//!
//! [`Temperatures`] holds one [`TempFilter`] for each kind of source and
//! publishes their values into a [`Watches`]. A consumer subscribes to the
//! watch of the value it needs and decides what a `None` means.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant};
use ubos_core::units::Temperature;
use ubos_temperature::{Reading, TempSource};

use crate::filter::TempFilter;
use crate::{TempSample, TempStats, WATCH_CAPACITY};

#[cfg(test)]
#[path = "tests/temps.rs"]
mod tests;

/// A published temperature. `None` means that no reading of the source
/// is inside the window of its filter.
pub type TempWatch = Watch<CriticalSectionRawMutex, Option<Temperature>, WATCH_CAPACITY>;

/// The published chip and hashboard temperatures. They travel together
/// so that a reader sees both kinds of one tick. `None` means that neither
/// kind has a reading inside the window of its filter.
pub type FilteredWatch = Watch<CriticalSectionRawMutex, Option<TempSample>, WATCH_CAPACITY>;

/// One watch for each published value.
pub struct Watches {
    /// The chip and the hashboard temperatures.
    pub filtered: FilteredWatch,
    /// The air around the board.
    pub ambient: TempWatch,
}

impl Watches {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            filtered: Watch::new(),
            ambient: Watch::new(),
        }
    }
}

impl Default for Watches {
    fn default() -> Self {
        Self::new()
    }
}

/// The filters of every published temperature. A reading of the control
/// board or of the power supply has no consumer but its metric, so it has
/// no filter.
pub struct Temperatures {
    hashboard: TempFilter,
    chip: TempFilter,
    ambient: TempFilter,
}

impl Temperatures {
    /// Filters for readings that arrive once in each `interval`.
    #[must_use]
    pub const fn for_interval(interval: Duration) -> Self {
        Self {
            hashboard: TempFilter::for_interval(interval),
            chip: TempFilter::for_interval(interval),
            ambient: TempFilter::for_interval(interval),
        }
    }

    /// Fold `reading` into the filter of its kind, whichever chain or
    /// sensor it names.
    pub fn update(&mut self, reading: Reading, now: Instant) {
        let filter = match reading.source {
            TempSource::Hashboard { .. } => &mut self.hashboard,
            TempSource::Chip { .. } => &mut self.chip,
            TempSource::Ambient => &mut self.ambient,
            TempSource::ControlBoard | TempSource::Psu => return,
        };
        filter.update(reading.value, now);
    }

    /// Fold the hottest and the mean die of the chains of a tick into
    /// the chip filter. A chain reads each chip once in a pass, so its
    /// dies reach the filter as the summary of every tick.
    pub(crate) fn update_dies(&mut self, dies: TempStats, now: Instant) {
        self.chip.update_stats(dies, now);
    }

    /// The chip and hashboard values at `now`, or `None` when neither has
    /// a reading inside its window.
    fn filtered(&self, now: Instant) -> Option<TempSample> {
        let chip = self.chip.value(now);
        let hashboard = self.hashboard.value(now);
        (chip.is_some() || hashboard.is_some()).then_some(TempSample { chip, hashboard })
    }

    /// Publish the chip and hashboard values into `watches`.
    pub fn publish_filtered(&self, now: Instant, watches: &Watches) {
        watches.filtered.sender().send(self.filtered(now));
    }

    /// Publish the ambient value into `watches`. It is apart from the
    /// chip and hashboard values because the loop reads the ambient
    /// sensor after they have gone out.
    pub fn publish_ambient(&self, now: Instant, watches: &Watches) {
        let ambient = self.ambient.value(now).map(|stats| stats.max);
        watches.ambient.sender().send(ambient);
    }
}
