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

//! The tuning targets of version 1.

use ubos_core::units::{Efficiency, Frequency, Power, Voltage};

/// ASIC frequency and voltage overrides.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FrequencyVoltageTarget {
    /// Target ASIC core frequency. The PLL grid steps by a fraction of
    /// a megahertz, so a megahertz field cannot name every point on it.
    /// It is stored and exchanged as integer kHz under `frequency_khz`.
    #[serde(rename = "frequency_khz")]
    pub frequency: Frequency,
    /// Core supply voltage, stored and exchanged as integer mV under
    /// `voltage_mv`.
    #[serde(rename = "voltage_mv")]
    pub voltage: Voltage,
}

/// The power a power tuner holds the machine at.
///
/// PSU input power, so it is the figure a site meters and bills, not
/// the hashboard draw. What range a machine accepts follows from its
/// supply and its boards, and the tuner that reads this is the only
/// thing that knows it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PowerTarget {
    /// Target PSU input power, stored and exchanged as integer mW under
    /// `input_power_mw`.
    #[serde(rename = "input_power_mw")]
    pub input_power: Power,
    /// Target input power per theoretical hashrate, which caps the clock
    /// the tuner ramps to. It is stored and exchanged as integer mJ/TH
    /// under `efficiency_mj_per_th`.
    #[serde(rename = "efficiency_mj_per_th")]
    pub efficiency: Efficiency,
}
