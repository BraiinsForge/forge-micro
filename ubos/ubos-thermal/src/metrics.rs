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

//! The pushes of every reading a sensor takes: one series for each
//! reading, under a name that says what the sensor measures.
//!
//! The series are in degrees Celsius. The store holds the
//! [`Temperature`] and writes it with integer math, so the exposition
//! path needs no software floating point on a board with no FPU.
//!
//! Metrics: `../../../docs/METRICS.md`.

use embedded_hal_async::i2c::SevenBitAddress;
use ubos_config::thermal::BOUNDS;
use ubos_core::index::{ChipAddress, HashboardIndex};
use ubos_core::units::Temperature;
use ubos_metrics::catalog::{AMBIENT_TEMPERATURE, CHIP_TEMPERATURE, HASHBOARD_LABEL, ID_LABEL};
use ubos_metrics::consts::Metrics;
use ubos_metrics::metric;
use ubos_temperature::{Reading, TempSource};

/// The default TTL of a metric is a multiple of the slowest push
/// interval. A sample interval above it would let a series expire between
/// two pushes, so the bound of the config must stay within it.
const _: () = assert!(
    *BOUNDS.sample_interval_ms.end() as u128 <= Metrics::SLOWEST_PUSH_INTERVAL.as_millis() as u128,
    "the thermal sample interval bound exceeds Metrics::SLOWEST_PUSH_INTERVAL"
);

/// Push one reading under the name and the labels that its source names,
/// or drop it when the metrics channel is full. The loop that calls this
/// must not wait on the metrics, and the next reading replaces a dropped
/// one.
pub(crate) fn publish_reading(reading: Reading) {
    match reading.source {
        TempSource::Hashboard { chain, address } => {
            publish_hashboard_reading(chain, address, reading.value);
        }
        TempSource::Chip { chain, chip } => publish_chip_reading(chain, chip, reading.value),
        TempSource::Ambient => metric!(AMBIENT_TEMPERATURE, reading.value).try_push(),
        TempSource::ControlBoard => {
            metric!("control_board_temperature_celsius", reading.value).try_push();
        }
        TempSource::Psu => metric!("psu_temperature_celsius", reading.value).try_push(),
    }
}

/// Push one reading of the circuit board of the hashboard `chain` under
/// the I2C address of the sensor that took it.
fn publish_hashboard_reading(chain: HashboardIndex, address: SevenBitAddress, value: Temperature) {
    metric!("pcb_temperature_celsius", value)
        .label(HASHBOARD_LABEL, chain)
        .label(ID_LABEL, format_args!("{address:#04x}"))
        .try_push();
}

/// Push one chip's reading under its address on the hashboard `chain`.
///
/// TODO: A board that read one chip both through a sensor diode and over
/// its chain would write this series from two places, each overwriting the
/// other. No board does today; one that does needs the series told apart.
fn publish_chip_reading(chain: HashboardIndex, chip: ChipAddress, value: Temperature) {
    metric!(CHIP_TEMPERATURE, value)
        .label(HASHBOARD_LABEL, chain)
        .label("chip_addr", chip)
        .try_push();
}
