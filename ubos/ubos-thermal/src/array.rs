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
//! A board describes its sensor topology as a `&'static [SensorDescriptor]`
//! table, then instantiates [`SensorArray`] with that table. On each sample,
//! `SensorArray` reads every sensor through the drivers in
//! `ubos-temperature`, folds the readings into the two values this crate
//! needs — a safety reading and a cooling-PID input — and publishes
//! every reading it took. Boards supply only the topology; all folding
//! logic lives here.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::units::Temperature;
use ubos_logging::{Debug2Format, error};
use ubos_temperature::{ReadingError, SensorDescriptor, TempSource};

use crate::metrics::publish_sensor_reading;
use crate::{Reading, Sample, Source, TemperatureSensor};

/// The chain of every board that carries an I2C sensor table: one
/// hashboard, on chain 0.
///
/// TODO: Take the chain from the board once a machine carries a sensor
/// table per hashboard.
const CHAIN: usize = 0;

/// Multi-sensor hashboard aggregator backed by a descriptor slice.
pub struct SensorArray<Bus> {
    bus: Bus,
    sensors: &'static [SensorDescriptor],
}

impl<Bus> SensorArray<Bus> {
    #[must_use]
    pub const fn new(bus: Bus, sensors: &'static [SensorDescriptor]) -> Self {
        Self { bus, sensors }
    }
}

impl<Bus> TemperatureSensor for SensorArray<Bus>
where
    Bus: I2c,
{
    async fn sample(&mut self) -> Reading {
        let mut chip: Option<Temperature> = None;
        let mut hashboard: Option<Temperature> = None;

        for desc in self.sensors {
            let address = desc.address();
            for result in desc.read_temperatures(&mut self.bus).await {
                let Ok(reading) = result.inspect_err(|error| log_fault(address, error)) else {
                    continue;
                };
                publish_sensor_reading(CHAIN, address, reading).await;
                match reading.source {
                    TempSource::Chip(_) => chip = chip.max(Some(reading.value)),
                    TempSource::Hashboard => hashboard = hashboard.max(Some(reading.value)),
                    // Published only: these gate no halt and drive no fan.
                    TempSource::Ambient | TempSource::ControlBoard | TempSource::Psu => {}
                }
            }
        }

        match (chip, hashboard) {
            (Some(chip), hashboard) => Reading::Measured(Sample {
                safety: chip,
                safety_source: Source::Chip,
                pid: hashboard,
            }),
            (None, Some(hashboard)) => Reading::Measured(Sample {
                safety: hashboard,
                safety_source: Source::Hashboard,
                pid: None,
            }),
            (None, None) => Reading::Unreadable,
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
