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

//! Unit tests for [`super::Accumulator`].

use core::num::NonZeroU8;

use ubos_asic::TemperatureSummary;
use ubos_core::units::Temperature;

use super::{Accumulator, TempStats};

#[test]
fn an_accumulator_answers_the_hottest_and_the_mean_reading() {
    let sensors = Accumulator::from(Temperature::from_millicelsius(50_000))
        .include(Temperature::from_millicelsius(56_000));
    let chain = TemperatureSummary {
        chips: NonZeroU8::new(2).expect("two chips"),
        mean: Temperature::from_millicelsius(52_000),
        lowest: Temperature::from_millicelsius(51_000),
        highest: Temperature::from_millicelsius(53_000),
    };

    assert_eq!(
        sensors.include(chain).stats(),
        TempStats {
            max: Temperature::from_millicelsius(56_000),
            mean: Temperature::from_millicelsius(52_500),
        }
    );
}
