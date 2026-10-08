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

//! Thermal monitoring configuration and validation.
//!
//! Re-exports [`Source`], the kinds of reading a machine takes,
//! [`TemperatureDescriptor`], a temperature for each of them, and
//! [`ThermalConfig`] for the thermal monitor loop from the live version of
//! the config schema, and defines their validation, [`ThermalBounds`] for
//! the accepted ranges, and the static [`BOUNDS`] constant.

use core::ops::RangeInclusive;

use ubos_asic::HashboardDescriptor;
use ubos_core::units::Temperature;

pub use crate::versions::live::{Source, TemperatureDescriptor, ThermalConfig};

impl Source {
    /// The `source` label of this kind in the metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hashboard => "hashboard",
            Self::Chip => "chip",
        }
    }
}

impl TemperatureDescriptor {
    /// The temperature of `source`, when the machine reads it.
    #[must_use]
    pub const fn temp(&self, source: Source) -> Option<Temperature> {
        match source {
            Source::Chip => self.chip,
            Source::Hashboard => self.hashboard,
        }
    }

    /// Whether `other` names the same kinds of reading, whatever its
    /// temperatures.
    #[must_use]
    pub const fn same_kinds(&self, other: &Self) -> bool {
        self.chip.is_some() == other.chip.is_some()
            && self.hashboard.is_some() == other.hashboard.is_some()
    }

    /// The halt thresholds `board` declares, one for each kind of
    /// reading the machine measures.
    #[must_use]
    pub const fn dangerous_of(board: &HashboardDescriptor) -> Self {
        Self {
            chip: board.dangerous_chip_temp,
            hashboard: board.dangerous_hashboard_temp,
        }
    }

    /// The highest of the temperatures, or `None` when there is none.
    #[must_use]
    pub fn max(&self) -> Option<Temperature> {
        self.chip.max(self.hashboard)
    }
}

/// Poll interval in milliseconds a board starts with. It is the same on
/// every board, so it lives here rather than on the board descriptor.
pub(crate) const DEFAULT_SAMPLE_INTERVAL_MS: u32 = 1000;

/// Resume threshold a board starts with. It is the same on every board,
/// so it lives here rather than on the board descriptor.
pub(crate) const DEFAULT_COOLDOWN_TEMP: Temperature = Temperature::from_celsius(45);

impl ThermalConfig {
    /// Validate all fields against [`BOUNDS`].
    ///
    /// Returns the first error found as a static operator-readable string.
    ///
    /// # Errors
    ///
    /// Returns `Err` when any field is out of range or any ordering invariant
    /// is violated: `cooldown_temp` below the highest dangerous temperature, and
    /// the hashboard threshold at or below the chip one.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !BOUNDS.sample_interval_ms.contains(&self.sample_interval_ms) {
            return Err("thermal.sample_interval_ms must be between 100 and 60000");
        }
        if let Some(hashboard) = self.dangerous.hashboard
            && !BOUNDS.dangerous_hashboard_temp.contains(&hashboard)
        {
            return Err("thermal.dangerous_hashboard_temp must be between 50000 and 110000");
        }
        if let Some(chip) = self.dangerous.chip
            && !BOUNDS.dangerous_chip_temp.contains(&chip)
        {
            return Err("thermal.dangerous_chip_temp must be between 50000 and 115000");
        }
        if let (Some(chip), Some(hashboard)) = (self.dangerous.chip, self.dangerous.hashboard)
            && hashboard > chip
        {
            return Err(
                "thermal.dangerous_hashboard_temp must not exceed thermal.dangerous_chip_temp",
            );
        }
        if !BOUNDS.cooldown_temp.contains(&self.cooldown_temp) {
            return Err("thermal.cooldown_temp must be between 20000 and 90000");
        }
        // TODO: Rework the cooldown the same way as the dangerous
        // temperatures, one for each kind. Held below the highest one only, a
        // halt judged on a lower one can resume on its next sample.
        if let Some(highest) = self.dangerous.max()
            && self.cooldown_temp >= highest
        {
            return Err(
                "thermal.cooldown_temp must be less than the highest dangerous temperature",
            );
        }
        Ok(())
    }
}

/// Accepted ranges for each [`ThermalConfig`] field.
///
/// Serializable so API endpoints can return the bounds alongside the
/// current config without a separate hard-coded schema.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThermalBounds {
    /// Accepted range for `sample_interval_ms` in milliseconds.
    pub sample_interval_ms: RangeInclusive<u32>,
    /// Accepted range for `dangerous_hashboard_temp`.
    pub dangerous_hashboard_temp: RangeInclusive<Temperature>,
    /// Accepted range for `dangerous_chip_temp`.
    pub dangerous_chip_temp: RangeInclusive<Temperature>,
    /// Accepted range for `cooldown_temp`.
    pub cooldown_temp: RangeInclusive<Temperature>,
}

/// Accepted value ranges for [`ThermalConfig`] fields.
pub const BOUNDS: ThermalBounds = ThermalBounds {
    sample_interval_ms: 100..=60_000,
    dangerous_hashboard_temp: Temperature::from_celsius(50)..=Temperature::from_celsius(110),
    dangerous_chip_temp: Temperature::from_celsius(50)..=Temperature::from_celsius(115),
    cooldown_temp: Temperature::from_celsius(20)..=Temperature::from_celsius(90),
};

/// Static bounds metadata for the thermal config section.
///
/// Carries the accepted ranges for [`ThermalConfig`] so the webui can
/// pull input `min`/`max` attributes from the same source the firmware
/// validates against.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThermalInfo {
    /// Accepted ranges for [`ThermalConfig`] fields.
    pub bounds: &'static ThermalBounds,
}

/// Bounds metadata served at `GET /api/system/thermal`.
pub const INFO: ThermalInfo = ThermalInfo { bounds: &BOUNDS };
