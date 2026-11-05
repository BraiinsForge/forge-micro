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

//! Version 1 of the config schema, the oldest that a build upgrades from.
//!
//! It defines every type of the schema, one file per domain of the
//! config. The capacities of its strings and lists are defined here as
//! well, because the schema hash does not cover them.

mod api;
mod cooling;
mod miner;
mod mining;
mod network;
mod pool;
mod thermal;

pub use api::ApiConfig;
pub use cooling::{CoolingConfig, FanMode, PidConfig, PidTarget, Statistic};
pub use miner::{MinerConfig, Tuning};
pub use mining::{FrequencyVoltageTarget, PowerTarget};
pub use network::{DNS_SERVERS, HOSTNAME_MAX, NetworkConfig, NetworkMode};
pub use pool::{MAX_POOLS, NAME_CAPACITY, PoolEntry, ProtocolKind};
pub use thermal::{Source, TemperatureDescriptor, ThermalConfig};

use super::{Versioned, schema_hash};

/// All runtime-tunable firmware parameters.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config {
    /// Ordered list of pool endpoints; the client tries them in order.
    pub pool: heapless::Vec<PoolEntry, MAX_POOLS>,
    /// Thermal monitoring parameters.
    pub thermal: ThermalConfig,
    /// Cooling loop parameters.
    pub cooling: CoolingConfig,
    /// Miner orchestration: the tuner kind and its target.
    pub miner: MinerConfig,
    /// HTTP API credentials.
    pub api: ApiConfig,
    /// Network interface configuration (IP assignment mode and hostname).
    pub network: NetworkConfig,
}

impl Versioned for Config {
    const HASH: [u8; 8] = [0x16, 0x5E, 0xA7, 0x41, 0x7A, 0x5F, 0xBA, 0x6C];
}

const _: () = assert!(
    u64::from_ne_bytes(schema_hash::<Config>()) == u64::from_ne_bytes(<Config as Versioned>::HASH),
    "The config schema is not the schema of version 1. Do the steps for a schema change in ubos/ubos-config/src/versions/mod.rs."
);
