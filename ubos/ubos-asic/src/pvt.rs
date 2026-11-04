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

//! The chain's PVT store: the voltage and temperature its chips report
//! about themselves, kept where the actor that reads the chain and the
//! loops that judge the readings can both reach it.
//!
//! The driver records into it from the serial actor; the thermal sensor
//! and the metrics read from it, on another executor or another core.
//! One critical-section mutex guards the whole store, the way the event
//! ring and the work watch cross cores, so no reading is ever torn, and
//! a cell that has never been written reads as absent rather than as
//! zero.
//!
//! What the store hands out is mostly the chain's aggregate: the thermal
//! loop needs the hottest and the mean die temperature the chain reported
//! and whether the chain is still reporting at all. One chip's die
//! temperature is handed out too, for the metric that publishes every
//! chip's reading.

use core::cell::RefCell;
use core::num::NonZeroU8;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Instant;
use ubos_core::units::Temperature;

/// Index of a chip on its chain, the byte the driver addresses it by.
pub type ChipIndex = u8;

#[cfg(test)]
#[path = "tests/pvt.rs"]
mod tests;

/// What one chip has reported about itself.
#[derive(Clone, Copy)]
struct ChipReadings {
    temperature_millicelsius: Option<i32>,
    voltage_microvolts: Option<i32>,
}

impl ChipReadings {
    const NONE: Self = Self {
        temperature_millicelsius: None,
        voltage_microvolts: None,
    };
}

struct Readings<const CHIPS: usize> {
    chips: [ChipReadings; CHIPS],
    /// When any chip last answered, absent until one has. The chain's
    /// only clock: a watcher asks whether readings are still arriving,
    /// never which chip they came from.
    last_answer: Option<Instant>,
}

/// Per-chip temperature and core voltage, read as the chain's aggregate.
pub struct ChainPvt<const CHIPS: usize> {
    readings: Mutex<CriticalSectionRawMutex, RefCell<Readings<CHIPS>>>,
}

impl<const CHIPS: usize> ChainPvt<CHIPS> {
    #[must_use]
    pub const fn new() -> Self {
        // A chain's chip count and the driver's population travel in one
        // byte, so a store past that can never be addressed; and an
        // empty store could never report anything, which is a wiring
        // bug and not a state.
        const {
            assert!(
                CHIPS >= 1 && CHIPS <= u8::MAX as usize,
                "the chain capacity must fit the count's byte"
            );
        }
        Self {
            readings: Mutex::new(RefCell::new(Readings {
                chips: [ChipReadings::NONE; CHIPS],
                last_answer: None,
            })),
        }
    }

    /// Record what one status turn read from `chip`; either quantity may
    /// be absent when its read failed.
    pub fn record(&self, chip: ChipIndex, celsius: Option<f64>, millivolts: Option<f64>) {
        self.readings.lock(|readings| {
            let mut readings = readings.borrow_mut();
            let Some(entry) = readings.chips.get_mut(usize::from(chip)) else {
                return;
            };
            // Scaled to integers for the store. Both conversions are
            // bounded well inside i32 by the ranges the driver clamps them
            // to.
            #[allow(clippy::cast_possible_truncation)]
            if let Some(value) = celsius {
                entry.temperature_millicelsius = Some((value * 1000.0) as i32);
            }
            #[allow(clippy::cast_possible_truncation)]
            if let Some(value) = millivolts {
                entry.voltage_microvolts = Some((value * 1000.0) as i32);
            }
            // A call that carried nothing leaves the clock where it
            // stands: a failed read is not the chain answering.
            if celsius.is_some() || millivolts.is_some() {
                readings.last_answer = Some(Instant::now());
            }
        });
    }

    /// When any chip last answered, absent until one has.
    #[must_use]
    pub fn last_answer(&self) -> Option<Instant> {
        self.readings.lock(|readings| readings.borrow().last_answer)
    }

    /// The die temperature `chip` last reported, or `None` until it has
    /// reported one.
    #[must_use]
    pub fn temperature(&self, chip: ChipIndex) -> Option<Temperature> {
        self.readings.lock(|readings| {
            readings
                .borrow()
                .chips
                .get(usize::from(chip))
                .and_then(|entry| entry.temperature_millicelsius)
                .map(Temperature::from_millicelsius)
        })
    }

    /// The hottest die the chain has reported, in millicelsius.
    #[must_use]
    pub fn hottest_millicelsius(&self) -> Option<i32> {
        self.readings.lock(|readings| {
            readings
                .borrow()
                .chips
                .iter()
                .filter_map(|entry| entry.temperature_millicelsius)
                .max()
        })
    }

    /// The chips that reported a temperature, with their sum and spread.
    /// The hottest and the mean drive the thermal loop; the spread says
    /// whether one die is running away from the rest.
    #[must_use]
    pub fn temperature_summary(&self) -> Option<TemperatureSummary> {
        self.readings.lock(|readings| {
            let mut total = 0i32;
            let mut lowest = i32::MAX;
            let mut highest = i32::MIN;
            let mut chips = 0u8;
            for value in readings
                .borrow()
                .chips
                .iter()
                .filter_map(|entry| entry.temperature_millicelsius)
            {
                total = total.saturating_add(value);
                lowest = lowest.min(value);
                highest = highest.max(value);
                chips += 1;
            }
            NonZeroU8::new(chips).map(|chips| TemperatureSummary {
                chips,
                total_millicelsius: total,
                lowest_millicelsius: lowest,
                highest_millicelsius: highest,
            })
        })
    }

    #[must_use]
    pub fn voltage_summary(&self) -> Option<VoltageSummary> {
        self.readings.lock(|readings| {
            let mut total = 0i32;
            let mut lowest = i32::MAX;
            let mut highest = i32::MIN;
            let mut chips = 0u8;
            for value in readings
                .borrow()
                .chips
                .iter()
                .filter_map(|entry| entry.voltage_microvolts)
            {
                total = total.saturating_add(value);
                lowest = lowest.min(value);
                highest = highest.max(value);
                chips += 1;
            }
            (chips > 0).then_some(VoltageSummary {
                chips,
                total_microvolts: total,
                lowest_microvolts: lowest,
                highest_microvolts: highest,
            })
        })
    }
}

impl<const CHIPS: usize> Default for ChainPvt<CHIPS> {
    fn default() -> Self {
        Self::new()
    }
}

/// The chips that reported a die temperature, with their sum and spread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TemperatureSummary {
    pub chips: NonZeroU8,
    pub total_millicelsius: i32,
    pub lowest_millicelsius: i32,
    pub highest_millicelsius: i32,
}

/// The chips that reported a core voltage, with their sum and spread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct VoltageSummary {
    pub chips: u8,
    pub total_microvolts: i32,
    pub lowest_microvolts: i32,
    pub highest_microvolts: i32,
}
