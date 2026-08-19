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

//! Board-temperature sensor trait and the hardware drivers behind it.
//! The LM75A, TMP102 and TMP461 drivers live in sibling modules, and
//! `array` folds a board's whole sensor table into the readings the
//! thermal layer needs. `ubos-thermal` owns the sample loop and the
//! gauge; board firmware crates supply their own die-sensor
//! `TemperatureSensor` impl.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod array;
pub mod lm75a;
pub mod tmp102;
pub mod tmp461;

pub use array::{SensorArray, SensorDescriptor, SensorKind};

use ubos_core::units::Temperature;

/// Source label marking a safety reading as the real die temperature rather
/// than a PCB-derived approximation. The thermal layer keys the die-specific
/// danger threshold off this exact label, so producer and consumer share it.
pub const CHIP_SOURCE: &str = "chip";

/// Internal die temperature sensor exposed by a board HAL.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    /// Read the current die temperature, or `None` if the reading
    /// failed. This is the value that gates thermal shutdown. Implementations
    /// may await on the underlying hardware conversion.
    async fn read(&mut self) -> Option<Temperature>;

    /// Temperature that should drive the cooling PID when it differs from
    /// the safety reading returned by [`read`](Self::read), cached from the
    /// most recent `read`. `None` means the PID uses the safety reading, so
    /// single-sensor boards need not implement it.
    fn pid_temperature(&self) -> Option<Temperature> {
        None
    }

    /// Source label for the safety reading, or `None` to publish the
    /// canonical gauge without a `source` label. Boards that report more
    /// than one chip-temperature estimate distinguish them this way.
    fn temperature_source(&self) -> Option<&'static str> {
        None
    }

    /// Source label for [`pid_temperature`](Self::pid_temperature).
    fn pid_temperature_source(&self) -> Option<&'static str> {
        None
    }

    /// Whether a sensor that has stopped reading should halt mining.
    ///
    /// True for a board whose sensor is independent of what it measures,
    /// where silence means the reading is gone and the board is running
    /// unwatched. False for one whose only thermometer is inside the
    /// chips it is trying to bring up, where silence is the normal state
    /// until the chain answers and halting on it stops the very work that
    /// would produce a reading.
    ///
    /// A board that returns false still logs the fault, still publishes
    /// it, and still gets the fan forced to full. It just is not stopped.
    fn halts_on_fault(&self) -> bool {
        true
    }
}
