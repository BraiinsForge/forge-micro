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
//! Defines [`Tuning`] and [`MinerConfig`], which select a tuner and
//! provide its target for each chip bring-up cycle.

use crate::mining::{FrequencyVoltageTarget, MiningBounds, PowerTarget};

/// Tuner selection and target parameters used at bring-up.
///
/// A variant names a kind of tuning and carries that kind's target.
/// Which tuner a machine runs for a kind is the firmware's answer, not
/// this crate's, and a machine need not answer every kind. Changing the
/// variant tears down the chip and restarts under the tuner the
/// firmware selects for it; changing only the target does not, because
/// the running tuner reads it at its next step.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Tuning {
    /// Hold one frequency and one rail voltage, reached by a ramp.
    /// The default.
    Fixed(FrequencyVoltageTarget),
    /// Hold a power draw, leaving the frequency and the rail to the
    /// tuner.
    Power(PowerTarget),
}

impl Default for Tuning {
    /// Factory defaults for the BMM101 board. Firmware builds its real
    /// per-board defaults via [`crate::Config::for_board`].
    fn default() -> Self {
        Self::Fixed(FrequencyVoltageTarget {
            frequency_khz: ubos_asic::BMM101_HASHBOARD.default_frequency.khz(),
            voltage_mv: ubos_asic::BMM101_HASHBOARD.default_voltage.mv(),
        })
    }
}

impl Tuning {
    /// Validate the selected tuner's parameters against `bounds`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a parameter falls outside its accepted range.
    pub fn validate(&self, bounds: &MiningBounds) -> Result<(), &'static str> {
        match self {
            Self::Fixed(target) => {
                if !bounds.frequency_khz.contains(&target.frequency_khz) {
                    return Err("miner.tuning.Fixed.frequency_khz outside the accepted range");
                }
                if !bounds.voltage_mv.contains(&target.voltage_mv) {
                    return Err("miner.tuning.Fixed.voltage_mv outside the accepted range");
                }
            }
            // A power target has no bound to check here. The range a
            // machine accepts follows from the supply and the boards it
            // carries, which `MiningBounds` does not describe, and the
            // tuner that reads the target is the only thing that knows
            // it.
            Self::Power(..) => {}
        }
        Ok(())
    }
}

/// Miner-level tuning configuration.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MinerConfig {
    /// Tuner variant and target selected for the next bring-up.
    pub tuning: Tuning,
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
