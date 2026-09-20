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

//! Hashboard descriptors.
//!
//! A [`HashboardDescriptor`] carries the chip's capability descriptor and
//! per-hashboard topology (series voltage domains, chip count, PSU
//! ceiling) so the tuner can read chip capabilities through a
//! hashboard-scoped seam. The static instances (such as
//! [`BMM101_HASHBOARD`]) name the concrete hashboards the firmware
//! supports; the consumer picks the matching [`crate::ChainDriver`]
//! implementation separately.

use ubos_core::units::{Frequency, Voltage};

use crate::ChipDescriptor;
// The built-in hashboard statics below are populated with the built-in
// bm13xx chip family; a board carrying another vendor's chip lives
// beside that vendor's own driver crate instead of here.
use crate::bm13xx::chip::{BM1362, BM1370};

#[cfg(test)]
#[path = "tests/board.rs"]
mod tests;

/// Per-board cooling profile. Grouped onto [`HashboardDescriptor`] so the
/// cooling defaults travel with the board rather than as loose fields.
/// The hashnode runs die-temp blind (a synthetic pcb+offset proxy), so it
/// targets a lower setpoint and pins a higher fan floor than the default
/// board.
#[derive(Clone, Copy)]
pub struct BoardCooling {
    /// Cooling PID setpoint in Celsius.
    pub target_temp_c: f32,
    /// Cooling PID fan-floor (minimum duty).
    pub min_fan_pwm: u8,
}

/// Hashboard-level descriptor. Carries the chip's capability descriptor
/// so the tuner can read chip capabilities through a hashboard-scoped
/// seam; per-hashboard topology fields land here alongside the code that
/// consumes them.
#[derive(Clone, Copy)]
pub struct HashboardDescriptor {
    /// Capability and identity descriptor of the chip populated on this
    /// hashboard. Selects the capability bounds the tuner drives; the
    /// consumer's driver selection is a separate concern this descriptor
    /// does not carry.
    pub chip: &'static ChipDescriptor,
    /// Number of voltage domains stacked in series on the rail. The
    /// PSU programs the sum across the stack, so the rail-side
    /// ceiling is `chip.max_chip_voltage * num_voltage_domains`.
    pub num_voltage_domains: u8,
    /// Number of chips on the serial chain. Drives the per-chip
    /// bring-up loops and the nonce-space division. Equals
    /// `num_voltage_domains` only when each chip is a single domain.
    pub num_chips: u8,
    /// Upper bound the PSU enforces on this board's rail. The
    /// `max_rail_voltage` accessor clamps the chip-stack product
    /// against this so `Psu::set_voltage` never returns
    /// `Error::OutOfRange` from a descriptor / PSU range mismatch.
    /// Matches the `end_mv()` of the `VoltageRange` the firmware
    /// configures on the board's PSU.
    // TODO: this field is PSU-specific but stays on the hashboard
    // descriptor for now; it should move onto BmmPsuDescriptor /
    // BfmPsuDescriptor once a board needs a rail ceiling that differs
    // from its hashboard's. ubos-psu already has descriptor types for
    // the PSU itself, but this field has not moved into them yet.
    pub max_psu_voltage: Voltage,
    /// Boot operating point, so a board comes up at its correct rail and
    /// clock from its descriptor alone, with no `.cargo/config.toml` env.
    pub default_voltage: Voltage,
    pub default_frequency: Frequency,
    /// Swarm color assigned to this device type for UI grouping.
    pub swarm_color: &'static str,
    /// Per-board cooling profile (PID setpoint and fan floor).
    pub cooling: BoardCooling,
    /// Die-temperature halt threshold in Celsius, read by
    /// `Config::for_board` into `ThermalConfig::dangerous_chip_temp_c`.
    /// Travels with the board because a chip family's safe junction
    /// ceiling is a property of the silicon, not a runtime tuning knob.
    pub dangerous_chip_temp_c: f32,
    /// Product name reported to external clients and the dashboard, sited
    /// here so the active board's name has one home.
    pub model: &'static str,
    /// 7-bit I2C address of the hashboard's operational-data EEPROM, on
    /// the same bus as the PSU/sensors. `None` when the address is not
    /// yet known; a `Some` value may still be tentative and pending
    /// hardware confirmation — see docs/EEPROM.md.
    pub eeprom_i2c_address: Option<u8>,
}

impl HashboardDescriptor {
    /// Open-core rail voltage: the per-chip maximum scaled by the
    /// number of series-stacked voltage domains, clamped against the
    /// PSU's configured upper bound. The tuner anchors the
    /// `V^2 * f` curve here before any clock is applied, and the
    /// rail walks down V as f climbs. Doubles as the tuning ceiling
    /// (`ubos-config` bounds), so there is no separate tuning-voltage
    /// field on the descriptor.
    #[must_use]
    pub const fn max_rail_voltage(&self) -> Voltage {
        let chip_stack = self
            .chip
            .max_chip_voltage
            .mv()
            .saturating_mul(self.num_voltage_domains as u32);
        let psu_max = self.max_psu_voltage.mv();
        let ceiling = if chip_stack < psu_max {
            chip_stack
        } else {
            psu_max
        };
        Voltage::from_mv(ceiling)
    }
}

/// Hashboard descriptor. The `max_psu_voltage` mirrors
/// `BmmPsuDescriptor::DEFAULT.dcdc_voltage_range` (`VRange2`, end
/// 1675 mV).
pub const BMM101_HASHBOARD: HashboardDescriptor = HashboardDescriptor {
    chip: &BM1362.chip,
    num_voltage_domains: 4,
    num_chips: 4,
    max_psu_voltage: Voltage::from_mv(1675),
    default_voltage: Voltage::from_mv(1400),
    default_frequency: Frequency::from_mhz(525),
    swarm_color: "blue",
    cooling: BoardCooling {
        target_temp_c: 67.0,
        min_fan_pwm: 20,
    },
    dangerous_chip_temp_c: 110.0,
    model: "miniminer",
    // Confirmed against BOS's AM2-S17 addressing
    // (`BASE_I2C_ADDRESS = 0x50`, single-hashboard `hashboard_address = 1`
    // resolves to `0x50 + 1 - 1`), which shares this same I2C bus with the
    // 0x40/0x43/0x46/0x08 PSU addresses `BmmPsuDescriptor::DEFAULT` uses.
    eeprom_i2c_address: Some(0x50),
};

/// Hashboard descriptor for the 4-chip BM1370 `BFMX4` (Braiins Forge Miner
/// x4) hashboard, carried on the Hashnode machine. Four BM1370 voltage
/// domains stack in series; the femto reference design ran two domains at
/// 2.4 V total (≈1.2 V per domain), so four need ≈4.8 V.
pub const BFMX4_HASHBOARD: HashboardDescriptor = HashboardDescriptor {
    chip: &BM1370.chip,
    num_voltage_domains: 4,
    num_chips: 4,
    max_psu_voltage: Voltage::from_mv(4800),
    default_voltage: Voltage::from_mv(4550),
    default_frequency: Frequency::from_mhz(600),
    swarm_color: "blue",
    cooling: BoardCooling {
        target_temp_c: 62.0,
        min_fan_pwm: 20,
    },
    dangerous_chip_temp_c: 110.0,
    model: "Braiins Forge Miner x4",
    // Tentative, unlike BMM101_HASHBOARD's 0x50: not yet cross-checked
    // against a parsed product-name match the way 0x50 was. See
    // docs/EEPROM.md.
    eeprom_i2c_address: Some(0x57),
};

/// The Forge Axe hashboard: four BM1370 in a single parallel voltage
/// domain on a 1.2 V rail, driven by a TPS53647 multiphase VRM.
pub const FORGE_AXE_HASHBOARD: HashboardDescriptor = HashboardDescriptor {
    chip: &BM1370.chip,
    num_voltage_domains: 1,
    num_chips: 4,
    // Caps max_rail_voltage below the chip's 1250 mV policy ceiling;
    // the tuner opens the cores at this 1.2 V rail.
    max_psu_voltage: Voltage::from_mv(1200),
    default_voltage: Voltage::from_mv(1150),
    default_frequency: Frequency::from_mhz(600),
    swarm_color: "blue",
    // Same cooling profile as the hashnode until tuned on hardware.
    cooling: BoardCooling {
        target_temp_c: 62.0,
        min_fan_pwm: 20,
    },
    dangerous_chip_temp_c: 110.0,
    model: "Forge Axe",
    eeprom_i2c_address: None,
};
