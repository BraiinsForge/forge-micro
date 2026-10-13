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
//! Defines [`ThermalConfig`] for the thermal monitor loop,
//! [`ThermalBounds`] for the accepted ranges, and the static
//! [`BOUNDS`] constant.

use core::ops::RangeInclusive;

/// Tunable parameters for the thermal monitor loop.
///
/// `sample_interval_ms` is stored as milliseconds for serde
/// simplicity; the runtime converts to `Duration` at the boundary.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ThermalConfig {
    /// Poll interval in milliseconds.
    pub sample_interval_ms: u32,
    /// Temperature in Celsius above which mining is halted when the safety
    /// reading is a hashboard reading. `Config::for_board` takes it from the
    /// board descriptor, and [`validate`](Self::validate) holds it at or below
    /// `dangerous_chip_temp_c`.
    pub dangerous_hashboard_temp_c: f32,
    /// Temperature in Celsius above which mining is halted when the safety
    /// reading is a real die temperature. This is the junction ceiling of
    /// the chip family, which `Config::for_board` takes from the board
    /// descriptor.
    pub dangerous_chip_temp_c: f32,
    /// Temperature in Celsius below which a halted miner may resume.
    pub cooldown_temp_c: f32,
}

/// Poll interval in milliseconds a board starts with. It is the same on
/// every board, so it lives here rather than on the board descriptor.
pub(crate) const DEFAULT_SAMPLE_INTERVAL_MS: u32 = 1000;

/// Resume threshold in Celsius a board starts with. It is the same on
/// every board, so it lives here rather than on the board descriptor.
pub(crate) const DEFAULT_COOLDOWN_TEMP_C: f32 = 45.0;

impl ThermalConfig {
    /// Validate all fields against [`BOUNDS`].
    ///
    /// Returns the first error found as a static operator-readable string.
    ///
    /// # Errors
    ///
    /// Returns `Err` when any field is out of range or any ordering invariant
    /// is violated: `cooldown_temp_c < dangerous_hashboard_temp_c`,
    /// `dangerous_hashboard_temp_c <= dangerous_chip_temp_c`.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !BOUNDS.sample_interval_ms.contains(&self.sample_interval_ms) {
            return Err("thermal.sample_interval_ms must be between 100 and 60000");
        }
        if !self.dangerous_hashboard_temp_c.is_finite() {
            return Err("thermal.dangerous_hashboard_temp_c must be a finite number");
        }
        if !BOUNDS
            .dangerous_hashboard_temp_c
            .contains(&self.dangerous_hashboard_temp_c)
        {
            return Err("thermal.dangerous_hashboard_temp_c must be between 50 and 110");
        }
        if !self.dangerous_chip_temp_c.is_finite() {
            return Err("thermal.dangerous_chip_temp_c must be a finite number");
        }
        if !BOUNDS
            .dangerous_chip_temp_c
            .contains(&self.dangerous_chip_temp_c)
        {
            return Err("thermal.dangerous_chip_temp_c must be between 50 and 115");
        }
        if self.dangerous_hashboard_temp_c > self.dangerous_chip_temp_c {
            return Err(
                "thermal.dangerous_hashboard_temp_c must not exceed thermal.dangerous_chip_temp_c",
            );
        }
        if !self.cooldown_temp_c.is_finite() {
            return Err("thermal.cooldown_temp_c must be a finite number");
        }
        if !BOUNDS.cooldown_temp_c.contains(&self.cooldown_temp_c) {
            return Err("thermal.cooldown_temp_c must be between 20 and 90");
        }
        if self.cooldown_temp_c >= self.dangerous_hashboard_temp_c {
            return Err(
                "thermal.cooldown_temp_c must be less than thermal.dangerous_hashboard_temp_c",
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
pub struct ThermalBounds {
    /// Accepted range for `sample_interval_ms` in milliseconds.
    pub sample_interval_ms: RangeInclusive<u32>,
    /// Accepted range for `dangerous_hashboard_temp_c` in Celsius.
    pub dangerous_hashboard_temp_c: RangeInclusive<f32>,
    /// Accepted range for `dangerous_chip_temp_c` in Celsius.
    pub dangerous_chip_temp_c: RangeInclusive<f32>,
    /// Accepted range for `cooldown_temp_c` in Celsius.
    pub cooldown_temp_c: RangeInclusive<f32>,
}

/// Accepted value ranges for [`ThermalConfig`] fields.
pub const BOUNDS: ThermalBounds = ThermalBounds {
    sample_interval_ms: 100..=60_000,
    dangerous_hashboard_temp_c: 50.0..=110.0,
    dangerous_chip_temp_c: 50.0..=115.0,
    cooldown_temp_c: 20.0..=90.0,
};

/// Static bounds metadata for the thermal config section.
///
/// Carries the accepted ranges for [`ThermalConfig`] so the webui can
/// pull input `min`/`max` attributes from the same source the firmware
/// validates against.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThermalInfo {
    /// Accepted ranges for [`ThermalConfig`] fields.
    pub bounds: &'static ThermalBounds,
}

/// Bounds metadata served at `GET /api/system/thermal`.
pub const INFO: ThermalInfo = ThermalInfo { bounds: &BOUNDS };
