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
//! hashboard-scoped seam. It also carries the values its model hands to
//! a new config, so a board and its defaults come from one constant. The
//! static instances (such as
//! [`BMM101_HASHBOARD`]) name the concrete hashboards the firmware
//! supports; the consumer picks the matching [`crate::ChainDriver`]
//! implementation separately.

use core::num::NonZeroU32;
use core::ops::RangeInclusive;

use ubos_config::{
    DefaultTuning, HashboardDefaults, MiningBounds, MiningInfo, PidTarget, Source, Statistic,
    TemperatureDescriptor, TunerInfo,
};
use ubos_core::index::ChipAddress;
use ubos_core::units::{Efficiency, FanPwm, Frequency, Power, Temperature, Voltage};
use ubos_temperature::SensorDescriptor;
use ubos_temperature::lm75a::Lm75a;
use ubos_temperature::tmp102::Tmp102;
use ubos_temperature::tmp461::Tmp461;
use ubos_temperature::tmp464::Tmp464;

use crate::ChipDescriptor;
// The built-in hashboard statics below are populated with the built-in
// bm13xx chip family; a board carrying another vendor's chip lives
// beside that vendor's own driver crate instead of here.
use crate::bm13xx::chip::{BM1362, BM1370};

#[cfg(test)]
#[path = "tests/board.rs"]
mod tests;

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
    /// Matches the `end()` of the `VoltageRange` the firmware
    /// configures on the board's PSU.
    // TODO: this field is PSU-specific but stays on the hashboard
    // descriptor for now; it should move onto BmmPsuDescriptor /
    // BfmPsuDescriptor once a board needs a rail ceiling that differs
    // from its hashboard's. ubos-psu already has descriptor types for
    // the PSU itself, but this field has not moved into them yet.
    pub max_psu_voltage: Voltage,
    /// The values a new config starts with on this hashboard: the boot
    /// operating point, the halt thresholds and the cooling profile. They
    /// travel with the model, so a board comes up at its correct rail and
    /// clock from its descriptor alone, and the firmware hands them to
    /// the config task as they stand here.
    pub defaults: HashboardDefaults,
    /// Swarm color assigned to this device type for UI grouping.
    pub swarm_color: &'static str,
    /// Product name reported to external clients and the dashboard, sited
    /// here so the active board's name has one home.
    pub model: &'static str,
    /// 7-bit I2C address of the hashboard's operational-data EEPROM, on
    /// the same bus as the PSU/sensors. `None` when the address is not
    /// yet known; a `Some` value may still be tentative and pending
    /// hardware confirmation — see docs/EEPROM.md.
    pub eeprom_i2c_address: Option<u8>,
    /// The temperature sensors on the hashboard.
    pub temp: &'static [SensorDescriptor],
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

    /// Accepted tuning ranges on this hashboard, and the machine's
    /// `tuners`. The frequency range is the chip's own, and the voltage
    /// ceiling is [`Self::max_rail_voltage`]. The config crate takes
    /// both as plain values, so the crate that owns the hardware reads
    /// it here. It panics as [`ubos_config::bounds`] does.
    #[must_use]
    pub const fn bounds(
        &self,
        tuners: &'static [TunerInfo],
        input_power: Option<RangeInclusive<Power>>,
    ) -> MiningBounds {
        ubos_config::bounds(
            self.chip.frequency_range.min()..=self.chip.frequency_range.max(),
            self.max_rail_voltage(),
            tuners,
            input_power,
        )
    }

    /// Static capability metadata of this hashboard: its chip model, its
    /// product name, its swarm color, its chip count and its defaults.
    /// Each firmware crate builds its `MINING_INFO` const from this. It
    /// panics as [`ubos_config::mining_info`] does.
    #[must_use]
    pub const fn mining_info(
        &self,
        bounds: &'static MiningBounds,
        default_tuning: DefaultTuning,
    ) -> MiningInfo {
        ubos_config::mining_info(
            self.chip.asic_model,
            self.model,
            self.swarm_color,
            self.num_chips,
            &self.defaults,
            bounds,
            default_tuning,
        )
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
    defaults: HashboardDefaults {
        frequency: Frequency::from_mhz(525),
        voltage: Voltage::from_mv(1400),
        // Measured on the bmm-pico.
        efficiency: Efficiency::from_mj_per_th(
            NonZeroU32::new(40_000).expect("BUG: efficiency is zero"),
        ),
        dangerous: TemperatureDescriptor {
            chip: None,
            hashboard: Some(Temperature::from_celsius(83)),
        },
        cooling_target: PidTarget {
            source: Source::Hashboard,
            statistic: Statistic::Max,
            temp: Temperature::from_celsius(60),
        },
        min_fan_pwm: FanPwm::from_percent(20),
        input_smoother_half_decay_secs: 0,
    },
    swarm_color: "blue",
    model: "miniminer",
    // Confirmed against BOS's AM2-S17 addressing
    // (`BASE_I2C_ADDRESS = 0x50`, single-hashboard `hashboard_address = 1`
    // resolves to `0x50 + 1 - 1`), which shares this same I2C bus with the
    // 0x40/0x43/0x46/0x08 PSU addresses `BmmPsuDescriptor::DEFAULT` uses.
    eeprom_i2c_address: Some(0x50),
    // The four LM75A board sensors, IC12D..IC12A at `0x49..=0x4c`.
    temp: &[
        SensorDescriptor::Lm75a(Lm75a::hashboard(0, 0x49)),
        SensorDescriptor::Lm75a(Lm75a::hashboard(0, 0x4a)),
        SensorDescriptor::Lm75a(Lm75a::hashboard(0, 0x4b)),
        SensorDescriptor::Lm75a(Lm75a::hashboard(0, 0x4c)),
    ],
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
    defaults: HashboardDefaults {
        frequency: Frequency::from_mhz(600),
        voltage: Voltage::from_mv(4550),
        // TODO: Replace this placeholder with a value measured on the board.
        efficiency: Efficiency::from_mj_per_th(
            NonZeroU32::new(15_000).expect("BUG: efficiency is zero"),
        ),
        dangerous: TemperatureDescriptor {
            chip: Some(Temperature::from_celsius(110)),
            hashboard: Some(Temperature::from_celsius(75)),
        },
        cooling_target: PidTarget {
            source: Source::Hashboard,
            statistic: Statistic::Max,
            temp: Temperature::from_celsius(47),
        },
        min_fan_pwm: FanPwm::from_percent(20),
        input_smoother_half_decay_secs: 0,
    },
    swarm_color: "blue",
    model: "Braiins Forge Miner x4",
    // Tentative, unlike BMM101_HASHBOARD's 0x50: not yet cross-checked
    // against a parsed product-name match the way 0x50 was. See
    // docs/EEPROM.md.
    eeprom_i2c_address: Some(0x57),
    // The TMP461 remote diode on the ASIC die plus two TMP102 PCB sensors.
    // The die reading and the hashboard readings, the TMP102 pair's and the
    // TMP461 local channel's, each gate thermal shutdown against the
    // threshold of their own kind, and the hashboard readings drive the
    // cooling PID.
    temp: &[
        SensorDescriptor::Tmp461(Tmp461::hashboard(0, 0x4E, ChipAddress::One(0))),
        SensorDescriptor::Tmp102(Tmp102::hashboard(0, 0x48)),
        SensorDescriptor::Tmp102(Tmp102::hashboard(0, 0x49)),
    ],
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
    defaults: HashboardDefaults {
        frequency: Frequency::from_mhz(600),
        voltage: Voltage::from_mv(1150),
        // Measured on the forge-axe.
        efficiency: Efficiency::from_mj_per_th(
            NonZeroU32::new(15_000).expect("BUG: efficiency is zero"),
        ),
        // Same hashboard halt threshold and cooling profile as the
        // hashnode until tuned on hardware.
        dangerous: TemperatureDescriptor {
            chip: Some(Temperature::from_celsius(110)),
            hashboard: Some(Temperature::from_celsius(75)),
        },
        cooling_target: PidTarget {
            source: Source::Hashboard,
            statistic: Statistic::Max,
            temp: Temperature::from_celsius(47),
        },
        min_fan_pwm: FanPwm::from_percent(20),
        input_smoother_half_decay_secs: 0,
    },
    swarm_color: "blue",
    model: "Forge Axe",
    eeprom_i2c_address: None,
    temp: &[
        SensorDescriptor::Tmp102(Tmp102::hashboard(0, 0x48)),
        SensorDescriptor::Tmp464(Tmp464::hashboard(
            0,
            0x4A,
            [
                Some(ChipAddress::One(0)),
                Some(ChipAddress::One(1)),
                Some(ChipAddress::One(2)),
                Some(ChipAddress::One(3)),
            ],
        )),
    ],
};
