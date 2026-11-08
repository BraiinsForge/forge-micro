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

//! The ambient gauge and the MCU die gauge.
//! [`run_thermal`](crate::run_thermal) is the only writer of both, so
//! the names and the pushes live beside that loop. Beside them is the
//! reading gauge, which the sensors write: one series for every reading a
//! sensor takes.
//!
//! Metrics: `../../../docs/METRICS.md`.

use core::fmt::Write as _;

use embedded_hal_async::i2c::SevenBitAddress;
use ubos_core::index::HashboardIndex;
use ubos_core::units::Temperature;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value};
use ubos_temperature::{ChipId, Reading, TempSource};

use crate::Source;
use crate::chain::chain_labels;

/// Every reading a sensor takes, under the `source` it measures. A
/// hashboard or chip reading also carries the `chain` it comes from, a
/// chip reading the `chip` it measures, and every other reading the
/// `addr` of the sensor that took it.
///
/// Prometheus convention prefers base SI units, which would
/// make `_celsius` the canonical suffix. The firmware targets the RP2040
/// among others, a Cortex-M0+ with no FPU, so a float-valued celsius
/// gauge would pull software floating-point into the exposition path.
/// Integer millicelsius preserves sub-degree resolution without
/// soft-float, and the name reflects the unit carried on the wire.
const READING_METRIC_NAME: &str = "board_temperature_millicelsius";

/// The `source` label of a hashboard reading.
const HASHBOARD_SOURCE: &str = Source::Hashboard.as_str();

/// The `source` label of a chip reading.
const CHIP_SOURCE: &str = Source::Chip.as_str();

/// The `source` label of a reading of the air around the board.
const AMBIENT_SOURCE: &str = "ambient";

/// The `source` label of a control-board reading.
const CONTROL_BOARD_SOURCE: &str = "control_board";

/// The `source` label of a power-supply reading.
const PSU_SOURCE: &str = "psu";

/// The ambient temperature around the board, where the board measures it.
/// Carries no label: a board reports one ambient reading or none. Integer
/// millicelsius for the reason [`READING_METRIC_NAME`] gives.
pub(crate) const AMBIENT_METRIC_NAME: &str = "board_ambient_temperature_millicelsius";

/// The temperature of the MCU die on the control board. Carries no
/// label: a board has one MCU. Integer millicelsius for the reason
/// [`READING_METRIC_NAME`] gives.
pub(crate) const MCU_METRIC_NAME: &str = "board_mcu_temperature_millicelsius";

/// Push one reading that the I2C sensor at `address` on `chain` took.
pub(crate) async fn publish_sensor_reading(
    chain: HashboardIndex,
    address: SevenBitAddress,
    reading: Reading,
) {
    match reading.source {
        TempSource::Hashboard => publish_hashboard_reading(chain, address, reading.value).await,
        TempSource::Chip(chip) => publish_chip_reading(chain, chip, reading.value).await,
        TempSource::Ambient => publish_board_reading(AMBIENT_SOURCE, address, reading.value).await,
        TempSource::ControlBoard => {
            publish_board_reading(CONTROL_BOARD_SOURCE, address, reading.value).await;
        }
        TempSource::Psu => publish_board_reading(PSU_SOURCE, address, reading.value).await,
    }
}

/// Push one hashboard reading under the I2C address of the sensor that
/// took it.
async fn publish_hashboard_reading(
    chain: HashboardIndex,
    address: SevenBitAddress,
    value: Temperature,
) {
    let mut labels = reading_labels(HASHBOARD_SOURCE, chain);
    let _ = labels.push(address_label(address));
    let value = Value::Int(i64::from(value.millicelsius()));
    Metric::with_labels(READING_METRIC_NAME, labels, value)
        .push()
        .await;
}

/// Push one reading of something on no chain under the I2C address of the
/// sensor that took it.
async fn publish_board_reading(source: &'static str, address: SevenBitAddress, value: Temperature) {
    let mut labels = Labels::new();
    let _ = labels.push(reading_source_label(source));
    let _ = labels.push(address_label(address));
    let value = Value::Int(i64::from(value.millicelsius()));
    Metric::with_labels(READING_METRIC_NAME, labels, value)
        .push()
        .await;
}

/// Push one chip's reading under its index on `chain`.
///
/// TODO: A board that read one chip both through a sensor diode and over
/// its chain would write this series from two places, each overwriting the
/// other. No board does today; one that does needs the series told apart.
pub(crate) async fn publish_chip_reading(chain: HashboardIndex, chip: ChipId, value: Temperature) {
    let mut chip_str = heapless::String::new();
    let _ = write!(chip_str, "{chip}");
    let mut labels = reading_labels(CHIP_SOURCE, chain);
    let _ = labels.push(Label {
        name: "chip",
        value: LabelValue::Short(chip_str),
    });
    let value = Value::Int(i64::from(value.millicelsius()));
    Metric::with_labels(READING_METRIC_NAME, labels, value)
        .push()
        .await;
}

/// The `source` label that every reading carries.
const fn reading_source_label(source: &'static str) -> Label {
    Label {
        name: "source",
        value: LabelValue::Static(source),
    }
}

/// The `source` and `chain` labels that every reading of a chain carries.
fn reading_labels(source: &'static str, chain: HashboardIndex) -> Labels {
    let mut labels = chain_labels(chain);
    let _ = labels.push(reading_source_label(source));
    labels
}

/// The `addr` label of the I2C sensor at `address`.
fn address_label(address: SevenBitAddress) -> Label {
    let mut address_str = heapless::String::new();
    let _ = write!(address_str, "{address:#04x}");
    Label {
        name: "addr",
        value: LabelValue::Short(address_str),
    }
}
