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

//! The hottest and the mean of one kind of reading, and the accumulator
//! that reduces a sensor's readings to them.

use core::num::NonZeroU16;
use ubos_asic::TemperatureSummary;
use ubos_core::units::Temperature;

use crate::Statistic;

#[cfg(test)]
#[path = "tests/stats.rs"]
mod tests;

/// The hottest and the mean of one kind of reading in one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempStats {
    pub max: Temperature,
    pub mean: Temperature,
}

impl TempStats {
    /// The reduction `statistic` names.
    #[must_use]
    pub const fn temp(self, statistic: Statistic) -> Temperature {
        match statistic {
            Statistic::Max => self.max,
            Statistic::Mean => self.mean,
        }
    }
}

/// The running hottest, sum and count of one kind of reading, which
/// [`stats`](Self::stats) reduces to [`TempStats`]. It starts from its
/// first reading, so a kind that took none has no accumulator and the
/// mean never divides by zero. The sum saturates as the chain's own
/// summary does, which no real count of readings comes near.
pub struct Accumulator {
    max: Temperature,
    total_millicelsius: i32,
    count: NonZeroU16,
}

impl From<Temperature> for Accumulator {
    fn from(value: Temperature) -> Self {
        Self {
            max: value,
            total_millicelsius: value.millicelsius(),
            count: const { NonZeroU16::new(1).unwrap() },
        }
    }
}

/// A chain counts once per die it read, so the board mean weighs each chain
/// by its dies. Its sum is rebuilt from its mean, whose rounding moves the
/// board mean by less than one millicelsius.
impl From<TemperatureSummary> for Accumulator {
    fn from(summary: TemperatureSummary) -> Self {
        Self {
            max: summary.highest,
            total_millicelsius: summary
                .mean
                .millicelsius()
                .saturating_mul(i32::from(summary.chips.get())),
            count: summary.chips.into(),
        }
    }
}

impl Accumulator {
    /// These readings together with `readings`.
    #[must_use]
    pub fn include(self, readings: impl Into<Self>) -> Self {
        let readings = readings.into();
        Self {
            max: self.max.max(readings.max),
            total_millicelsius: self
                .total_millicelsius
                .saturating_add(readings.total_millicelsius),
            count: self.count.saturating_add(readings.count.get()),
        }
    }

    /// The hottest and the mean of the readings taken.
    #[must_use]
    pub fn stats(self) -> TempStats {
        let mean = self.total_millicelsius / i32::from(self.count.get());
        TempStats {
            max: self.max,
            mean: Temperature::from_millicelsius(mean),
        }
    }
}
