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

//! Tuning policies driven by the miner through [`ubos_tuner::Tuner`].
//!
//! Policies calculate plans. The miner owns initialization, telemetry collection,
//! command ordering, cancellation, and teardown. Serial actors and the PSU own
//! their confirmed settings.

use ubos_core::units::{Frequency, Voltage};

/// Tuner-facing snapshot of the active frequency and voltage target.
///
/// `Copy` so the miner can pass it by value into the trait methods
/// without lifetime-managing a borrow against its own `Config`
/// subscription.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TuningConfig {
    pub frequency: Frequency,
    pub voltage: Voltage,
}

impl From<&ubos_config::TuningConfig> for TuningConfig {
    fn from(config: &ubos_config::TuningConfig) -> Self {
        Self {
            frequency: Frequency::from_mhz(config.frequency_mhz),
            voltage: Voltage::from_mv(config.voltage_mv),
        }
    }
}

/// Configuration belongs to the runner, outside the shared policy contract.
/// Each policy declares its algorithm and target conversion together.
pub(crate) trait ConfiguredTuner<const CHAINS: usize>: ubos_tuner::Tuner<CHAINS> {
    const ALGORITHM: ubos_config::TuningAlgorithm;

    fn target(config: &ubos_config::Config) -> Self::Target;
}

pub mod fixed;
