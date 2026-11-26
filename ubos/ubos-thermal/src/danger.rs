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

//! The dangerous temperature flag.
//!
//! [`Danger`] sets the flag when a reading reaches the limit of its kind,
//! and clears it when the kinds that set it have cooled or when the
//! operator acknowledges it. The flag is `true` while mining must stay
//! halted.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the loop does not run the flag yet")
)]

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::Instant;
use ubos_config::TemperatureDescriptor;
use ubos_core::units::Temperature;
use ubos_logging::{error, info};
use ubos_temperature::{Reading, TempSource};

use crate::Source;
use crate::WATCH_CAPACITY;
use crate::temps::Temperatures;

/// The watch that the flag is published into.
pub(crate) type FlagWatch = Watch<CriticalSectionRawMutex, bool, WATCH_CAPACITY>;

#[cfg(test)]
#[path = "tests/danger.rs"]
mod tests;

/// The kinds that the flag judges.
const KINDS: [Source; 2] = [Source::Chip, Source::Hashboard];

/// The kind that a reading of `source` is judged as, or `None` for a
/// source that the flag does not judge.
const fn kind_of(source: TempSource) -> Option<Source> {
    match source {
        TempSource::Chip { .. } => Some(Source::Chip),
        TempSource::Hashboard { .. } => Some(Source::Hashboard),
        TempSource::Ambient | TempSource::ControlBoard | TempSource::Psu => None,
    }
}

/// The kinds whose readings have set the flag since it was last clear.
/// The flag waits for a reading of each of them, because a cool kind says
/// nothing about a hot kind that is no longer read.
struct Tripped {
    chip: bool,
    hashboard: bool,
}

impl Tripped {
    const NONE: Self = Self {
        chip: false,
        hashboard: false,
    };

    /// Record that a reading of `kind` reached its limit.
    const fn mark(&mut self, kind: Source) {
        match kind {
            Source::Chip => self.chip = true,
            Source::Hashboard => self.hashboard = true,
        }
    }

    /// Whether a reading of `kind` set the flag.
    const fn holds(&self, kind: Source) -> bool {
        match kind {
            Source::Chip => self.chip,
            Source::Hashboard => self.hashboard,
        }
    }

    /// Whether every kind that set the flag has a reading inside its
    /// window at `now`.
    fn is_read(&self, temperatures: &Temperatures, now: Instant) -> bool {
        KINDS
            .into_iter()
            .all(|kind| !self.holds(kind) || temperatures.stats(kind, now).is_some())
    }
}

/// The state of the dangerous temperature flag.
pub(crate) struct Danger {
    /// Whether the flag is set. The watch takes it on each edge only, so
    /// a subscriber wakes once for each.
    is_set: bool,
    watch: &'static FlagWatch,
    /// The limit of each kind, from `ThermalConfig::dangerous`.
    limits: TemperatureDescriptor,
    tripped: Tripped,
    /// Whether a reading reached its limit since the last tick. The die
    /// of one chip goes into no filter, so the filters of that tick can
    /// read cool while the machine is hot.
    exceeded: bool,
}

impl Danger {
    /// A clear flag that publishes into `watch` and judges against
    /// `limits`.
    #[must_use]
    pub(crate) const fn new(watch: &'static FlagWatch, limits: TemperatureDescriptor) -> Self {
        Self {
            is_set: false,
            watch,
            limits,
            tripped: Tripped::NONE,
            exceeded: false,
        }
    }

    /// Publish the flag as it stands, so a consumer that starts later
    /// reads a value.
    pub(crate) fn publish(&self) {
        self.watch.sender().send(self.is_set);
    }

    /// Judge against `limits` from now on. A flag that is set stays set
    /// until a tick or the operator clears it.
    pub(crate) const fn set_limits(&mut self, limits: TemperatureDescriptor) {
        self.limits = limits;
    }

    /// Set or clear the flag and publish the edge.
    fn set(&mut self, set: bool) {
        self.is_set = set;
        self.watch.sender().send(set);
    }

    /// Set the flag if `reading` reaches the limit of its kind. A hot
    /// reading does not wait for a tick.
    pub(crate) fn on_reading(&mut self, reading: Reading) {
        if let Some(kind) = kind_of(reading.source) {
            self.on_value(kind, reading.value);
        }
    }

    /// Set the flag if `value` reaches the limit of `kind`. A kind with no
    /// limit never sets it. The chains hand their dies over as one summary
    /// of each sample, and its maximum comes here as a chip value like any
    /// other.
    pub(crate) fn on_value(&mut self, kind: Source, value: Temperature) {
        if self.limits.temp(kind).is_some_and(|limit| value >= limit) {
            self.tripped.mark(kind);
            self.trip(value);
        }
    }

    /// Set the flag for `value`, which reached its limit, and log the
    /// edge. A flag that is already set stays quiet.
    fn trip(&mut self, value: Temperature) {
        self.exceeded = true;
        if self.is_set {
            return;
        }
        error!("thermal: dangerous temperature {} °C, mining halted", value);
        crate::events::dangerous_temperature(value);
        self.set(true);
    }

    /// The hottest fresh value at `now` of the kinds that have a limit,
    /// or `None` when no such kind has a value. A kind with no limit
    /// cannot hold the flag up.
    fn hottest_judged(&self, temperatures: &Temperatures, now: Instant) -> Option<Temperature> {
        KINDS
            .into_iter()
            .filter(|kind| self.limits.temp(*kind).is_some())
            .filter_map(|kind| temperatures.stats(kind, now))
            .map(|fresh| fresh.max)
            .max()
    }

    /// Clear the flag if every kind that set it is fresh and every fresh
    /// value of a kind with a limit is at or below `cooldown`. The chips
    /// give no reading after the rail is cut, so a flag that a chip set
    /// stays for the operator. A tick that took a reading at its limit
    /// clears nothing.
    pub(crate) fn on_tick(
        &mut self,
        temperatures: &Temperatures,
        cooldown: Temperature,
        now: Instant,
    ) {
        let exceeded = core::mem::take(&mut self.exceeded);
        if exceeded || !self.is_set || !self.tripped.is_read(temperatures, now) {
            return;
        }
        if let Some(hottest) = self
            .hottest_judged(temperatures, now)
            .filter(|hottest| *hottest <= cooldown)
        {
            info!("thermal: cooled to {} °C", hottest);
            crate::events::temperature_cooled(hottest);
            self.tripped = Tripped::NONE;
            self.set(false);
        }
    }

    /// Clear the flag on the operator's word. Nothing holds it against
    /// the operator but a reading at its limit in this tick, which keeps
    /// the flag and drops the acknowledge.
    pub(crate) fn acknowledge(&mut self) {
        if !self.is_set || self.exceeded {
            return;
        }
        info!("thermal: the operator cleared the dangerous temperature");
        self.tripped = Tripped::NONE;
        self.set(false);
    }
}
