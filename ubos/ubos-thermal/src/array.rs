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
//! needs — a safety reading and a cooling-PID estimate — and publishes a
//! per-sensor metric for every sensor that has a named gauge. Boards supply
//! only the topology; all folding logic lives here.

use core::fmt::Write as _;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::Temperature;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};
use ubos_temperature::{CHIP_SOURCE, SensorDescriptor, SensorKind, TemperatureSensor, read_sensor};

#[cfg(test)]
#[path = "tests/array.rs"]
mod tests;

const APPROXIMATED_SOURCE: &str = "approximated";

/// Per-sensor gauge name for a driver, or `None` to publish no
/// per-sensor metric. The match is exhaustive, so a new `SensorKind`
/// fails this crate's build until it states which of the two it is.
const fn sensor_metric_name(kind: SensorKind) -> Option<&'static str> {
    match kind {
        SensorKind::Lm75a => Some("board_lm75a_temperature_millicelsius"),
        SensorKind::Tmp102 | SensorKind::Tmp461 => None,
    }
}

/// Fused board temperatures for one sample.
struct Fused {
    safety: Option<Temperature>,
    safety_source: Option<&'static str>,
    pid: Option<Temperature>,
}

/// Pure fold. `die` = hottest trustworthy die reading, `est` = hottest
/// PCB-plus-offset estimate, `has_die_capable` = the table contains at
/// least one die-wired sensor. Picks the safety value (real die if any,
/// else the estimate), tags its source, and exposes the estimate as the
/// PID input only when it is a distinct second series.
fn fuse(die: Option<i32>, est: Option<i32>, has_die_capable: bool) -> Fused {
    match die {
        Some(mc) => Fused {
            safety: Some(Temperature::from_millicelsius(mc)),
            safety_source: Some(CHIP_SOURCE),
            pid: est.map(Temperature::from_millicelsius),
        },
        None => Fused {
            safety: est.map(Temperature::from_millicelsius),
            // "approximated" only when there is a real die sensor to
            // contrast against; a board with no die sensor publishes the
            // canonical unlabeled gauge instead.
            safety_source: if has_die_capable && est.is_some() {
                Some(APPROXIMATED_SOURCE)
            } else {
                None
            },
            pid: None,
        },
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
    pid_temp: Option<Temperature>,
    safety_source: Option<&'static str>,
}

impl<Bus> SensorArray<Bus> {
    #[must_use]
    pub const fn new(bus: Bus, sensors: &'static [SensorDescriptor]) -> Self {
        Self {
            bus,
            sensors,
            pid_temp: None,
            safety_source: None,
        }
    }
}

impl<Bus> TemperatureSensor for SensorArray<Bus>
where
    Bus: I2c,
{
    async fn read(&mut self) -> Option<Temperature> {
        let mut die: Option<i32> = None;
        let mut est: Option<i32> = None;
        let mut has_die_capable = false;

        for desc in self.sensors {
            let reading = read_sensor(desc, &mut self.bus).await;
            if desc.attached_chip.is_some() {
                has_die_capable = true;
                // Die-wired sensor: only its remote diode feeds the safety
                // value. The local channel is read only so its I2C faults are
                // logged; the value is not a board PCB reference, so it is
                // neither used nor folded into the estimate.
                if let Some(mc) = reading.remote {
                    die = Some(die.map_or(mc, |cur| cur.max(mc)));
                }
            } else if let Some(local) = reading.local {
                let adjusted = local + desc.pcb_to_die_offset_mc;
                est = Some(est.map_or(adjusted, |cur| cur.max(adjusted)));
                if let Some(name) = sensor_metric_name(desc.kind) {
                    publish_sensor(name, desc.address, adjusted).await;
                }
            }
        }

        let fused = fuse(die, est, has_die_capable);
        self.pid_temp = fused.pid;
        self.safety_source = fused.safety_source;
        fused.safety
    }

    fn pid_temperature(&self) -> Option<Temperature> {
        self.pid_temp
    }

    fn temperature_source(&self) -> Option<&'static str> {
        self.safety_source
    }

    fn pid_temperature_source(&self) -> Option<&'static str> {
        self.pid_temp.map(|_| APPROXIMATED_SOURCE)
    }
}
