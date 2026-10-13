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

//! The chip temperature gauge and the ambient gauge.
//! [`run_thermal`](crate::run_thermal) is the only writer of both, so
//! the names and the pushes live beside that loop.
//!
//! Metrics: `../../../docs/METRICS.md`.

use ubos_core::units::Temperature;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

use crate::Source;

/// Gauge name. Prometheus convention prefers base SI units, which would
/// make `_celsius` the canonical suffix. The firmware targets the RP2040
/// among others, a Cortex-M0+ with no FPU, so a float-valued celsius
/// gauge would pull software floating-point into the exposition path.
/// Integer millicelsius preserves sub-degree resolution without
/// soft-float, and the name reflects the unit carried on the wire.
const METRIC_NAME: &str = "board_chip_temperature_millicelsius";

/// The ambient temperature around the board, where the board measures it.
/// Carries no label: a board reports one ambient reading or none. Integer
/// millicelsius for the reason [`METRIC_NAME`] gives.
const AMBIENT_METRIC_NAME: &str = "board_ambient_temperature_millicelsius";

/// The `source` label for a reading. The two label strings live here and
/// nowhere else, so no comparison outside this module can turn on one.
const fn source_label(source: Source) -> &'static str {
    match source {
        Source::Hashboard => "hashboard",
        Source::Chip => "chip",
    }
}

/// Push a chip-temperature reading, tagged with a `source` label so a
/// board that reports both a hashboard and a chip reading keeps them as
/// distinct series under one metric name.
pub(crate) async fn publish_chip_temperature(temp: Temperature, source: Source) {
    let value = Value::Int(i64::from(temp.millicelsius()));
    let source = source_label(source);
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "source",
        value: LabelValue::Static(source),
    });
    push(Metric::with_labels(METRIC_NAME, labels, value)).await;
}

/// Push the ambient temperature around the board. What the reading is
/// and is not for: [`crate::AmbientSensor`].
pub(crate) async fn publish_ambient_temperature(temp: Temperature) {
    push(Metric::with_no_labels(
        AMBIENT_METRIC_NAME,
        Value::Int(i64::from(temp.millicelsius())),
    ))
    .await;
}
