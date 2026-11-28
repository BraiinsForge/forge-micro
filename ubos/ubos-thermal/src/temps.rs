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
use ubos_config::{PidTarget, Source, Statistic};
use ubos_core::units::Temperature;
use ubos_temperature::{Reading, TempSource};

use crate::danger::FlagWatch;
use crate::filter::TempFilter;
use crate::{TempSample, TempStats, WATCH_CAPACITY};

#[cfg(test)]
#[path = "tests/temps.rs"]
mod tests;

/// A published temperature. `None` means that no reading of the source
/// is inside the window of its filter.
pub(crate) type TempWatch = Watch<CriticalSectionRawMutex, Option<Temperature>, WATCH_CAPACITY>;

/// The temperature that the cooling PID follows, with the selection that
/// picked it. The selection travels with the value because the target can
/// change between the tick that selects and the reader that steers, and
/// the reader then sees that the value answers another target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PidTemperature {
    /// The kind of reading that the statistic is taken over.
    pub source: Source,
    /// The reduction of the readings of that kind.
    pub statistic: Statistic,
    /// The value, or `None` when that kind has no reading inside the
    /// window of its filter.
    pub temp: Option<Temperature>,
}

/// The published temperature that the cooling PID follows.
pub(crate) type PidWatch = Watch<CriticalSectionRawMutex, PidTemperature, WATCH_CAPACITY>;

/// The published chip and hashboard temperatures. They travel together
/// so that a reader sees both kinds of one tick. `None` means that neither
/// kind has a reading inside the window of its filter.
pub(crate) type FilteredWatch = Watch<CriticalSectionRawMutex, Option<TempSample>, WATCH_CAPACITY>;

/// One watch for each published value.
pub(crate) struct Watches {
    /// The chip and the hashboard temperatures.
    pub(crate) filtered: FilteredWatch,
    /// The temperature that the cooling PID follows.
    pub(crate) pid: PidWatch,
    /// The air around the board.
    pub(crate) ambient: TempWatch,
    /// The dangerous temperature flag.
    pub(crate) danger: FlagWatch,
}

impl Watches {
    pub(crate) const fn new() -> Self {
        Self {
            filtered: Watch::new(),
            pid: Watch::new(),
            ambient: Watch::new(),
            danger: Watch::new(),
        }
    }
}

/// The filters of every published temperature. A reading of the control
/// board or of the power supply has no consumer but its metric, so it has
/// no filter.
pub(crate) struct Temperatures {
    hashboard: TempFilter,
    chip: TempFilter,
    ambient: TempFilter,
}

impl Temperatures {
    pub(crate) const fn new() -> Self {
        Self {
            hashboard: TempFilter::new(),
            chip: TempFilter::new(),
            ambient: TempFilter::new(),
        }
    }

    /// End this sample in every filter, after its values were published.
    pub(crate) fn end_sample(&mut self) {
        self.hashboard.end_sample();
        self.chip.end_sample();
        self.ambient.end_sample();
    }

    /// Fold `reading` into the filter of its kind, whichever chain or
    /// sensor it names.
    pub(crate) fn update(&mut self, reading: Reading) {
        let filter = match reading.source {
            TempSource::Hashboard { .. } => &mut self.hashboard,
            TempSource::Chip { .. } => &mut self.chip,
            TempSource::Ambient => &mut self.ambient,
            TempSource::Mcu | TempSource::ControlBoard | TempSource::Psu => return,
        };
        filter.update(reading.value);
    }

    /// Fold the hottest and the mean die of the chains of a sample into
    /// the chip filter. A chain reads each chip once in a pass, so its
    /// dies reach the filter as the summary of every sample.
    pub(crate) fn update_dies(&mut self, dies: TempStats) {
        self.chip.update_stats(dies);
    }

    /// The chip and hashboard values, or `None` when neither has
    /// a reading inside its window.
    fn filtered(&self) -> Option<TempSample> {
        let chip = self.chip.value();
        let hashboard = self.hashboard.value();
        (chip.is_some() || hashboard.is_some()).then_some(TempSample { chip, hashboard })
    }

    /// The stats of `source`, or `None` when it has no reading
    /// inside its window.
    pub(crate) fn stats(&self, source: Source) -> Option<TempStats> {
        self.filtered()?.temp_stats(source)
    }

    /// Publish the chip and hashboard values and the ambient value into
    /// `watches`.
    pub(crate) fn publish(&self, watches: &Watches) {
        watches.filtered.sender().send(self.filtered());
        let ambient = self.ambient.value().map(|stats| stats.max);
        watches.ambient.sender().send(ambient);
    }

    /// Publish the temperature that the cooling PID follows into
    /// `watches`: the statistic of `target` over the readings of its kind,
    /// or `None` when that kind has no reading inside its window. The
    /// selection is made here because thermal holds every filter, so the
    /// cooling loop wakes on the one value it steers by.
    pub(crate) fn publish_pid(&self, target: PidTarget, watches: &Watches) {
        let PidTarget {
            source, statistic, ..
        } = target;
        let temp = self
            .filtered()
            .and_then(|filtered| filtered.temp_stats(source))
            .map(|stats| stats.temp(statistic));
        watches.pid.sender().send(PidTemperature {
            source,
            statistic,
            temp,
        });
    }
}
