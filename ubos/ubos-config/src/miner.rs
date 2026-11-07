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

//! Miner orchestration configuration.
//!
//! Re-exports [`Tuning`] and [`MinerConfig`], which select a tuner and
//! provide its target for each chip bring-up cycle, from the live
//! version of the config schema, and defines their defaults and
//! validation.

use crate::mining::{DefaultTuning, FrequencyVoltageTarget, MiningBounds, PowerTarget};

pub use crate::versions::live::{MinerConfig, Tuning};

impl Default for Tuning {
    /// Factory defaults for the BMM101 board. Firmware builds its real
    /// per-board defaults via [`crate::Config::for_board`].
    fn default() -> Self {
        DefaultTuning::Fixed.tuning(&ubos_asic::BMM101_HASHBOARD)
    }
}

impl Tuning {
    /// Validate the selected tuner and its parameters against `bounds`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no tuner in `bounds` reads this variant, or a
    /// parameter falls outside its accepted range.
    pub fn validate(&self, bounds: &MiningBounds) -> Result<(), &'static str> {
        if !bounds.tuners.iter().any(|tuner| tuner.reads(self)) {
            return Err("miner.tuning selects a tuner this machine does not offer");
        }
        match self {
            Self::Fixed(target) => {
                if !bounds.frequency.contains(&target.frequency) {
                    return Err("miner.tuning.Fixed.frequency_khz outside the accepted range");
                }
                if !bounds.voltage.contains(&target.voltage) {
                    return Err("miner.tuning.Fixed.voltage_mv outside the accepted range");
                }
            }
            Self::Power(target) => {
                if !bounds.accepts_input_power(target.input_power) {
                    return Err("miner.tuning.Power.input_power_mw outside the accepted range");
                }
            }
        }
        Ok(())
    }

    /// This tuning with every field moved to the nearest end of its
    /// range in `bounds`.
    ///
    /// A stored tuning meets ranges the firmware narrowed after it was
    /// stored. Clamping keeps the operator's choice of tuner and the rest
    /// of their config, where failing validation would replace it all.
    #[must_use]
    pub fn clamped(self, bounds: &MiningBounds) -> Self {
        fn clamp<T: Ord + Copy>(value: T, range: &core::ops::RangeInclusive<T>) -> T {
            value.clamp(*range.start(), *range.end())
        }
        match self {
            Self::Fixed(target) => Self::Fixed(FrequencyVoltageTarget {
                frequency: clamp(target.frequency, &bounds.frequency),
                voltage: clamp(target.voltage, &bounds.voltage),
            }),
            Self::Power(target) => Self::Power(PowerTarget {
                input_power: match &bounds.input_power {
                    Some(range) if !bounds.accepts_input_power(target.input_power) => {
                        clamp(target.input_power, range)
                    }
                    _ => target.input_power,
                },
                ..target
            }),
        }
    }
}

impl MinerConfig {
    /// Validate the miner config.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the selected tuner's parameters are invalid.
    pub fn validate(&self, bounds: &MiningBounds) -> Result<(), &'static str> {
        self.tuning.validate(bounds)
    }
}
