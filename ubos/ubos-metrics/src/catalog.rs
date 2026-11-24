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

//! The series names and the label names that more than one push site
//! uses.
//!
//! A name that one site pushes is a literal at that site. A name or a
//! label that several sites share is declared here once, so the sites
//! cannot drift apart and a rename is one line.
//!
//! Metrics: `../../../docs/METRICS.md`.

use ubos_core::units::{Power, Temperature};

use crate::name::Name;

/// The label of the hashboard a series belongs to. Its value is the
/// index of the hashboard from 0, the chain index of the REST API.
pub const HASHBOARD_LABEL: &str = "hashboard";

/// The label of the pool a series belongs to. Its value is the index of
/// the pool slot from 0.
pub const POOL_LABEL: &str = "pool";

/// The label of the sensor that took a reading. Its value is the I2C
/// address of the sensor, such as `0x48`.
pub const ID_LABEL: &str = "id";

/// The label that tells apart two readings of one quantity, such as the
/// input and the output power of the supply.
pub const TYPE_LABEL: &str = "type";

/// The label that tells apart the statistics of one quantity, such as
/// `min`, `mean` and `max`, or `oldest` and `newest` for a set of ages.
pub const STAT_LABEL: &str = "stat";

/// The temperature of the air around the board. A board with one
/// ambient sensor pushes it with no label, and a board that reads the
/// air through its sensor table adds [`ID_LABEL`].
pub const AMBIENT_TEMPERATURE: Name<Temperature> = Name::new("ambient_temperature_celsius");

/// The spread of the chip temperatures of one hashboard, one series for
/// each statistic under [`STAT_LABEL`]. Each chip reading has a series of
/// its own, and this one shows whether one chip runs away from the rest.
pub const CHIP_TEMPERATURE_SUMMARY: Name<Temperature> =
    Name::new("chip_temperature_summary_celsius");

/// The power of the supply, one series for each side under
/// [`TYPE_LABEL`]: `input` and `output`.
pub const PSU_POWER: Name<Power> = Name::new("psu_power_watts");
