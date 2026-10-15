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

//! Coverage for the key budget: every metric the tree renders must fit
//! [`super::KEY_CAPACITY`], which is not checked at compile time for a
//! metric the constant does not name.

use super::{
    KEY_CAPACITY, LONGEST_RENDERED_KEY, Label, LabelValue, Labels, Metric, MetricKey, Value,
};

/// Build the key for a metric carrying the labels given, in the order
/// given, so a test states the call `ubos-thermal` makes rather than the
/// string it renders to.
fn key_for(name: &'static str, labels: &[(&'static str, &'static str)]) -> Option<MetricKey> {
    let mut built = Labels::new();
    for (name, value) in labels {
        assert!(
            built
                .push(Label {
                    name,
                    value: LabelValue::Static(value),
                })
                .is_ok(),
            "test labels fit MAX_LABELS"
        );
    }
    MetricKey::from_metric(&Metric::with_labels(name, built, Value::Int(0)))
}

/// The per-chain die-temperature spread carries two labels and the
/// longest name in the tree. It overran the cap once and was dropped at
/// runtime, so it is asserted to encode rather than left to the `warn!`.
#[test]
fn the_per_chain_temperature_spread_key_fits_the_budget() {
    for stat in ["lowest", "mean", "highest"] {
        assert!(
            key_for(
                "hashboard_chain_chip_temperature_millicelsius",
                &[("chain", "0"), ("stat", stat)],
            )
            .is_some(),
            "the {stat} series does not encode within KEY_CAPACITY"
        );
    }
}

/// The constant the capacity is asserted against must be the string
/// `from_metric` actually builds, or the compile-time check guards
/// nothing.
#[test]
fn the_longest_key_constant_matches_what_from_metric_renders() {
    let key = key_for(
        "board_temperature_millicelsius",
        &[("source", "hashboard"), ("chain", "0"), ("addr", "0x48")],
    )
    .expect("the longest key must encode");
    assert_eq!(key.0.as_str(), LONGEST_RENDERED_KEY);
}

/// A key one byte past the cap is dropped rather than truncated: a
/// truncated key would collide with another metric's.
#[test]
fn a_key_past_the_budget_is_refused() {
    const OVERLONG: &str = "x";
    let name: &'static str =
        "this_name_is_deliberately_longer_than_the_key_capacity_allows_for_any_metric_at_all";
    assert!(name.len() > KEY_CAPACITY);
    assert!(key_for(name, &[("label", OVERLONG)]).is_none());
}
