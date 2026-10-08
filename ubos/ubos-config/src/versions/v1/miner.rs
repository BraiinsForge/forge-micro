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

//! The miner config of version 1.

use super::mining::{FrequencyVoltageTarget, PowerTarget};

/// Tuner selection and target parameters used at bring-up.
///
/// A variant names a kind of tuning and carries that kind's target.
/// Which tuner a machine runs for a kind is the firmware's answer, not
/// this crate's, and a machine need not answer every kind: the firmware
/// lists the tuners it offers in
/// [`MiningBounds::tuners`](crate::MiningBounds::tuners). Changing the
/// variant tears down the chip and restarts under the tuner the
/// firmware selects for it; changing only the target does not, because
/// the running tuner reads it at its next step.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Tuning {
    /// Hold one frequency and one rail voltage, reached by a ramp.
    Fixed(FrequencyVoltageTarget),
    /// Hold a power draw, leaving the frequency and the rail to the
    /// tuner.
    Power(PowerTarget),
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MinerConfig {
    /// Tuner variant and target selected for the next bring-up.
    pub tuning: Tuning,
}
