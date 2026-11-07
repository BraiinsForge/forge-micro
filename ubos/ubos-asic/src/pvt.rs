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
use ubos_core::units::{Temperature, Voltage};

/// Index of a chip on its chain, the byte the driver addresses it by.
pub type ChipIndex = u8;

#[cfg(test)]
#[path = "tests/pvt.rs"]
mod tests;

/// What one chip has reported about itself.
#[derive(Clone, Copy)]
struct ChipReadings {
    temperature: Option<Temperature>,
    voltage: Option<Voltage>,
}

impl ChipReadings {
    const NONE: Self = Self {
        temperature: None,
        voltage: None,
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
    pub fn record(
        &self,
        chip: ChipIndex,
        temperature: Option<Temperature>,
        voltage: Option<Voltage>,
    ) {
        self.readings.lock(|readings| {
            let mut readings = readings.borrow_mut();
            let Some(entry) = readings.chips.get_mut(usize::from(chip)) else {
                return;
            };
            if let Some(value) = temperature {
                entry.temperature = Some(value);
            }
            if let Some(value) = voltage {
                entry.voltage = Some(value);
            }
            // A call that carried nothing leaves the clock where it
            // stands: a failed read is not the chain answering.
            if temperature.is_some() || voltage.is_some() {
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
                .and_then(|entry| entry.temperature)
        })
    }

    /// The hottest die the chain has reported.
    #[must_use]
    pub fn hottest(&self) -> Option<Temperature> {
        self.readings.lock(|readings| {
            readings
                .borrow()
                .chips
                .iter()
                .filter_map(|entry| entry.temperature)
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
            let mut range: Option<(Temperature, Temperature)> = None;
            let mut chips = 0u8;
            for die in readings
                .borrow()
                .chips
                .iter()
                .filter_map(|entry| entry.temperature)
            {
                total = total.saturating_add(die.millicelsius());
                range = Some(range.map_or((die, die), |(lowest, highest)| {
                    (lowest.min(die), highest.max(die))
                }));
                chips += 1;
            }
            let (lowest, highest) = range?;
            let chips = NonZeroU8::new(chips)?;
            Some(TemperatureSummary {
                chips,
                mean: Temperature::from_millicelsius(total / i32::from(chips.get())),
                lowest,
                highest,
            })
        })
    }

    /// The chips that reported a core voltage, with their sum and spread.
    #[must_use]
    pub fn voltage_summary(&self) -> Option<VoltageSummary> {
        self.readings.lock(|readings| {
            let mut total = 0u32;
            let mut range: Option<(Voltage, Voltage)> = None;
            let mut chips = 0u8;
            for value in readings
                .borrow()
                .chips
                .iter()
                .filter_map(|entry| entry.voltage)
            {
                total = total.saturating_add(value.mv());
                range = Some(range.map_or((value, value), |(lowest, highest)| {
                    (lowest.min(value), highest.max(value))
                }));
                chips += 1;
            }
            let (lowest, highest) = range?;
            Some(VoltageSummary {
                chips,
                total: Voltage::from_mv(total),
                lowest,
                highest,
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
    pub mean: Temperature,
    pub lowest: Temperature,
    pub highest: Temperature,
}

/// The chips that reported a core voltage, with their sum and spread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct VoltageSummary {
    pub chips: u8,
    pub total: Voltage,
    pub lowest: Voltage,
    pub highest: Voltage,
}
