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

//! The thermal config of version 1.

use ubos_core::units::Temperature;

/// What a temperature reading measures. `metrics` in `ubos-thermal`
/// turns it into the `source` label.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Source {
    /// A sensor on the hashboard.
    Hashboard,
    /// A chip's own die.
    Chip,
}

/// A temperature for each kind of reading, `None` for a kind the machine
/// does not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TemperatureDescriptor {
    /// The dies of the chips.
    pub chip: Option<Temperature>,
    /// The sensors on the hashboard.
    pub hashboard: Option<Temperature>,
    // NOTE: Ambient, ControlBoard and Psu can join as fields of their own
    // once a reading of theirs is judged.
}

/// Tunable parameters for the thermal monitor loop. Temperatures are
/// in millidegrees Celsius.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "ThermalConfigJson", into = "ThermalConfigJson")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThermalConfig {
    /// Poll interval in milliseconds.
    pub sample_interval_ms: u32,
    /// Temperatures above which mining is halted, one for each kind of
    /// reading the machine measures. The die threshold is the junction
    /// ceiling of the chip family. `Config::for_board` takes them from the
    /// hashboard defaults, and [`validate`](Self::validate) holds the
    /// hashboard one at or below the chip one.
    pub dangerous: TemperatureDescriptor,
    /// Temperature below which a halted miner may resume.
    pub cooldown_temp: Temperature,
}

/// The stored and the JSON shape of [`ThermalConfig`]. A threshold is
/// absent on a machine that does not read its kind, which keeps the field
/// names the API has always carried.
#[derive(Clone, serde::Serialize, serde::Deserialize, postcard_schema::Schema)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThermalConfigJson {
    /// Poll interval in milliseconds.
    sample_interval_ms: u32,
    /// Temperature above which mining is halted when the safety reading is
    /// a hashboard reading, or `null` on a machine that reads none. The
    /// board descriptor sets it, and validation holds it at or below
    /// `dangerous_chip_temp`.
    dangerous_hashboard_temp: Option<Temperature>,
    /// Temperature above which mining is halted when the safety reading is
    /// a real die temperature, or `null` on a machine that reads none. This
    /// is the junction ceiling of the chip family, which the board
    /// descriptor sets.
    dangerous_chip_temp: Option<Temperature>,
    /// Temperature below which a halted miner may resume.
    cooldown_temp: Temperature,
}

/// The schema of the bytes stored, which are those of [`ThermalConfigJson`].
impl postcard_schema::Schema for ThermalConfig {
    const SCHEMA: &'static postcard_schema::schema::NamedType = ThermalConfigJson::SCHEMA;
}

impl TryFrom<ThermalConfigJson> for ThermalConfig {
    type Error = &'static str;

    fn try_from(json: ThermalConfigJson) -> Result<Self, Self::Error> {
        let dangerous = TemperatureDescriptor {
            chip: json.dangerous_chip_temp,
            hashboard: json.dangerous_hashboard_temp,
        };
        if dangerous.chip.is_none() && dangerous.hashboard.is_none() {
            return Err("thermal must set dangerous_chip_temp, dangerous_hashboard_temp, or both");
        }
        Ok(Self {
            sample_interval_ms: json.sample_interval_ms,
            dangerous,
            cooldown_temp: json.cooldown_temp,
        })
    }
}

impl From<ThermalConfig> for ThermalConfigJson {
    fn from(config: ThermalConfig) -> Self {
        Self {
            sample_interval_ms: config.sample_interval_ms,
            dangerous_hashboard_temp: config.dangerous.hashboard,
            dangerous_chip_temp: config.dangerous.chip,
            cooldown_temp: config.cooldown_temp,
        }
    }
}
