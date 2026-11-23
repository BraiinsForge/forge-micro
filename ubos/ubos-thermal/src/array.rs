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

//! Descriptor-driven multi-sensor hashboard temperature aggregator.
//!
//! A board describes its sensor topology as `&'static` [`SensorDescriptor`]
//! entries, then instantiates [`SensorArray`] with them. On each sample,
//! `SensorArray` reads every sensor through the drivers in
//! `ubos-temperature`, folds the chip and the hashboard readings each into
//! their hottest and their mean, and publishes every reading it took.
//! Boards supply only the topology; all folding logic lives here.

use embassy_time::{Duration, with_timeout};
use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_logging::{Debug2Format, error};
use ubos_temperature::{ReadingError, Sensor, SensorDescriptor, TempSource};

use crate::{Accumulator, Handle, Reading, TempSample, TemperatureSensor};

/// The longest that the read of one sensor may take. A sensor that does not
/// answer in this time is passed over, so it cannot stop the reads of the
/// sensors behind it.
const READ_TIMEOUT: Duration = Duration::from_millis(250);

/// Multi-sensor hashboard aggregator over a board's sensor table of up
/// to `N` sensors.
pub struct SensorArray<const N: usize, Bus> {
    sensors: heapless::Vec<Sensor<Bus>, N>,
    /// Where each reading is submitted, so thermal filters it and writes
    /// its series.
    thermal: Handle,
}

impl<const N: usize, Bus: I2c> SensorArray<N, Bus> {
    /// One [`Sensor`] for each of `sensors`, each read through a device
    /// of its own from `bus_device`. Each reading goes to `thermal`.
    ///
    /// # Panics
    ///
    /// Panics when `sensors` holds more than `N` entries.
    #[must_use]
    pub fn new(
        sensors: impl IntoIterator<Item = &'static SensorDescriptor>,
        thermal: Handle,
        mut bus_device: impl FnMut() -> Bus,
    ) -> Self {
        Self {
            sensors: sensors
                .into_iter()
                .map(|descriptor| Sensor::new(descriptor, bus_device()))
                .collect(),
            thermal,
        }
    }
}

impl<const N: usize, Bus> TemperatureSensor for SensorArray<N, Bus>
where
    Bus: I2c,
{
    async fn sample(&mut self) -> Reading {
        let mut chip: Option<Accumulator> = None;
        let mut hashboard: Option<Accumulator> = None;

        for sensor in &mut self.sensors {
            let address = sensor.descriptor().address();
            let Ok(readings) = with_timeout(READ_TIMEOUT, sensor.read_temperatures()).await else {
                error!(
                    "temperature: {:#x} gave no answer in {} ms",
                    address,
                    READ_TIMEOUT.as_millis()
                );
                continue;
            };
            for result in readings {
                let Ok(reading) = result.inspect_err(|error| log_fault(address, error)) else {
                    continue;
                };
                self.thermal.submit(reading).await;
                match (reading.source, reading.value) {
                    (TempSource::Chip { .. }, temp) => {
                        chip = chip
                            .map(|acc| acc.include(temp))
                            .or_else(|| Some(temp.into()));
                    }
                    (TempSource::Hashboard { .. }, temp) => {
                        hashboard = hashboard
                            .map(|acc| acc.include(temp))
                            .or_else(|| Some(temp.into()));
                    }
                    // Published only: these gate no halt and drive no fan.
                    (TempSource::Ambient | TempSource::ControlBoard | TempSource::Psu, _) => {}
                }
            }
        }

        match (chip, hashboard) {
            (None, None) => Reading::Unreadable,
            (chip, hashboard) => Reading::Measured(TempSample {
                chip: chip.map(Accumulator::stats),
                hashboard: hashboard.map(Accumulator::stats),
            }),
        }
    }
}

/// Log the fault that kept a channel of the sensor at `address` from a
/// reading. Free because `ReadingError` belongs to `ubos-temperature`.
fn log_fault<E: core::fmt::Debug>(address: SevenBitAddress, error: &ReadingError<E>) {
    error!(
        "temperature: {:?} {:#x} error: {:?}",
        Debug2Format(&error.source),
        address,
        Debug2Format(&error.fault),
    );
}
