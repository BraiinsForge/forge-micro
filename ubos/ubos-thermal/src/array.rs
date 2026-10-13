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
//! needs — a safety reading and a cooling-PID input — and publishes a
//! per-sensor metric for every sensor that has a named gauge. Boards supply
//! only the topology; all folding logic lives here.

use core::fmt::Write as _;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::Temperature;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};
use ubos_temperature::{SensorDescriptor, SensorKind, read_sensor};

use crate::{Reading, Sample, Source, TemperatureSensor};

/// Per-sensor gauge name for a driver, or `None` to publish no
/// per-sensor metric. The match is exhaustive, so a new `SensorKind`
/// fails this crate's build until it states which of the two it is.
const fn sensor_metric_name(kind: SensorKind) -> Option<&'static str> {
    match kind {
        SensorKind::Lm75a => Some("board_lm75a_temperature_millicelsius"),
        SensorKind::Tmp102 | SensorKind::Tmp461 => None,
    }
}

/// Publish a per-sensor metric labeled by I2C address.
async fn publish_sensor(name: &'static str, address: u8, millicelsius: i32) {
    let mut address_str = heapless::String::<24>::new();
    let _ = write!(address_str, "{address:#04x}");
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "addr",
        value: LabelValue::Short(address_str),
    });
    push(Metric::with_labels(
        name,
        labels,
        Value::Int(i64::from(millicelsius)),
    ))
    .await;
}

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
            let reading = read_sensor(desc, &mut self.bus).await;
            if desc.attached_chip.is_some() {
                // Die-wired sensor: its remote diode reads the die.
                if let Some(mc) = reading.remote {
                    chip = chip.max(Some(Temperature::from_millicelsius(mc)));
                }
            }
            if let Some(local) = reading.local {
                hashboard = hashboard.max(Some(Temperature::from_millicelsius(local)));
                if let Some(name) = sensor_metric_name(desc.kind) {
                    publish_sensor(name, desc.address, local).await;
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
