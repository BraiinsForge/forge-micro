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
//! Defines [`TuningAlgorithm`] and [`MinerConfig`], which name the
//! bring-up strategy the mining task constructs at each chip bring-up
//! cycle.

/// Tuner algorithm the miner constructs at bring-up.
///
/// `ubos-mining` reads this to decide which `Tuner` implementation
/// to construct at bring-up. A live change tears down the chip and
/// restarts under the new tuner — switching algorithms requires a
/// full PSU + reset cycle, not an in-place swap.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TuningAlgorithm {
    /// Open-loop frequency ramp built on the BM1362 PLL calculator
    /// and the `HashCountingNumber` formula. Reaches arbitrary targets
    /// and is the default tuner.
    #[default]
    Fixed,
}

/// Miner-level orchestration. Separate from `TuningConfig` because
/// it does not describe what the tuner aims for — it describes which
/// tuner instance the miner constructs.
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
    /// Tuner algorithm selected for the next bring-up.
    pub algorithm: TuningAlgorithm,
}

impl MinerConfig {
    /// Validate the miner config.
    ///
    /// The algorithm field is an enum with no out-of-range values;
    /// this method exists for symmetry with the other per-domain
    /// validators so `Config::validate` and `ConfigUpdate::validate`
    /// can call it without special-casing.
    ///
    /// # Errors
    ///
    /// Always returns `Ok(())` for now.
    pub fn validate(&self) -> Result<(), &'static str> {
        Ok(())
    }
}
