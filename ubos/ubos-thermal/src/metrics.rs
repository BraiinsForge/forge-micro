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

//! The chip temperature gauge. [`run_thermal`](crate::run_thermal) is
//! its only writer, so the name and the push live beside that loop.
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

/// The `source` label for a reading. The two label strings live here and
/// nowhere else, so no comparison outside this module can turn on one.
const fn source_label(source: Source) -> Option<&'static str> {
    match source {
        Source::Sole => None,
        Source::Chip => Some("chip"),
        Source::Approximated => Some("approximated"),
    }
}

/// Push a chip-temperature reading, tagged with a `source` label so a
/// board that reports more than one chip-temperature estimate keeps them
/// as distinct series under one metric name.
pub(crate) async fn publish_chip_temperature(temp: Temperature, source: Source) {
    let value = Value::Int(i64::from(temp.millicelsius()));
    let Some(source) = source_label(source) else {
        push(Metric::with_no_labels(METRIC_NAME, value)).await;
        return;
    };
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "source",
        value: LabelValue::Static(source),
    });
    push(Metric::with_labels(METRIC_NAME, labels, value)).await;
}
