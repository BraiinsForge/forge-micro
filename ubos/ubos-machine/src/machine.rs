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

//! Machine composition: control board plus hashboard, PSU, and sensors.

use ubos_asic::HashboardDescriptor;
use ubos_core::units::{Current, Power, Temperature, Voltage};
use ubos_psu::sic45x::{LoopScale, Sic45xConfig};
use ubos_psu::tps53647::{Phases, Tps53647Config};
use ubos_psu::{BfmPsuDescriptor, BmmPsuDescriptor, ForgeAxePsuDescriptor};
use ubos_temperature::tmp102::Tmp102;
use ubos_temperature::{SensorDescriptor, TempSource};

use crate::control_board::{
    CONTROL_BOARD_BMM_ADAPTER, CONTROL_BOARD_ESP32S3, CONTROL_BOARD_FORGE_AXE,
    CONTROL_BOARD_HASHNODE, CONTROL_BOARD_RP2040, CONTROL_BOARD_RP2350, ControlBoardDescriptor,
};

/// The most chains one machine drives, re-exported from the crate that
/// owns the per-chain table it sizes. Every descriptor here asserts its
/// `chains` fits under it at compile time; a machine with more chains
/// raises it there and nowhere else.
pub use ubos_metrics::chain::MAX_CHAINS;

/// A complete miner: the control board plus the hashboard, PSU, and
/// temperature-sensor group it drives.
///
/// Identity and capability metadata is not carried here; the REST API
/// serves it from the per-board `HashboardDescriptor::mining_info` const
/// fn, which reads it from the hashboard descriptor so a second chip
/// model reports its own values.
///
/// Generic over `Psu`: no code handles a `MachineDescriptor` generically
/// — every board names its own concrete
/// `MachineDescriptor<BmmPsuDescriptor>`/`MachineDescriptor<BfmPsuDescriptor>`
/// const, so the PSU family is fixed at compile time per board rather
/// than needing a sum type unwrapped at each call site.
#[derive(Clone, Copy)]
pub struct MachineDescriptor<Psu: Copy> {
    /// Machine model name. Not consumed yet; scaffolding for the
    /// planned identity reporting.
    pub model: &'static str,
    /// The board the firmware runs on.
    pub control_board: ControlBoardDescriptor,
    /// The hashboard the machine drives. Every chain of the machine
    /// carries this same board, because one firmware image drives one
    /// chip model.
    pub hashboard: HashboardDescriptor,
    /// How many hashboard connectors the machine has. Each is a chain
    /// with its own serial actor, and every chain is enumerated at each
    /// bring-up, so a connector with no board answers zero chips and is
    /// marked absent.
    pub chains: usize,
    /// The PSU powering the hashboard rail.
    pub psu: Psu,
    /// The temperature sensors on the PSU. The machine holds them for a
    /// PSU descriptor that has them, because it is generic over the PSU.
    pub psu_temp: &'static [SensorDescriptor],
}

impl<Psu: Copy> MachineDescriptor<Psu> {
    /// How many temperature sensors the machine reads.
    #[must_use]
    pub const fn sensor_count(&self) -> usize {
        self.hashboard.temp.len() + self.psu_temp.len()
    }

    /// The temperature sensors the machine reads: those on the hashboard,
    /// then those on the PSU.
    pub fn sensors(&self) -> impl Iterator<Item = &'static SensorDescriptor> + use<Psu> {
        self.hashboard.temp.iter().chain(self.psu_temp)
    }
}

impl MachineDescriptor<BmmPsuDescriptor> {
    /// Compose the miniminer machine on a given control board. Every
    /// miniminer shares the same hashboard, PSU, and temperature-sensor
    /// topology; only the control board varies.
    #[must_use]
    pub const fn miniminer(control_board: ControlBoardDescriptor) -> Self {
        const CHAINS: usize = 1;
        const _: () = assert!(
            CHAINS <= MAX_CHAINS,
            "the miniminer has more chains than MAX_CHAINS"
        );
        Self {
            model: "miniminer",
            control_board,
            hashboard: ubos_asic::BMM101_HASHBOARD,
            chains: CHAINS,
            psu: BmmPsuDescriptor::DEFAULT,
            psu_temp: &[],
        }
    }
}

/// The miniminer on the W6100-EVB-Pico control board.
pub const MINIMINER_RP2040: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_RP2040);

/// The miniminer on the RP2350 control board.
pub const MINIMINER_RP2350: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_RP2350);

/// The miniminer on the BMM adapter control board.
pub const MINIMINER_BMM_ADAPTER: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_BMM_ADAPTER);

/// The miniminer on the ESP32-S3 control board.
pub const MINIMINER_ESP32S3: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_ESP32S3);

/// PSU input power a miniminer's power target may ask for, from the
/// lowest to the highest the BMM101 supply delivers.
pub const MINIMINER_INPUT_POWER: core::ops::RangeInclusive<Power> =
    Power::from_mw(19_000)..=Power::from_mw(67_000);

/// `SIC45x` configuration for the Hashnode 4-domain 4.8 V rail.
///
/// `loop_scale` is `Scale0_25` per the `SIC45x` page VOUT-range table: the
/// 3.3 V to 5.0 V range selects register `0xE802`, whose LINEAR11 decode
/// is 0.25.
const HASHNODE_SIC: Sic45xConfig = Sic45xConfig {
    address: 0x10,
    // Idle at the 0.6 V pre-open-core floor, matching BOS PARAMS_4X1. The
    // tuner programs the 4.8 V operating voltage before enumerating the chips.
    initial_voltage: Voltage::from_mv(600),
    loop_scale: LoopScale::Scale0_25,
    // OV fault 5.0 V with headroom above the 4.8 V operating ceiling, per
    // BOS PARAMS_4X1. BOS sets its OV warn at 4.85 V, but this driver has no
    // warn field.
    vout_ov_fault: Voltage::from_mv(5000),
    vout_uv_fault: Voltage::from_mv(700),
    vin_ov_fault: Voltage::from_mv(22_000),
    vin_uv_warn: Voltage::from_mv(9_000),
    // Actual SIC450 output limit is 40A, we set a lower to protect the chips.
    // Maximum achieved output current was 25.9A.
    iout_oc_fault: Current::from_ma(28_000),
    iout_oc_warn: Current::from_ma(26_000),
    // Input-current warning. 16A was the maximum achieved in practice.
    // Warning only; the device has no IIN fault trip.
    iin_oc_warn: Current::from_ma(16_000),
    ot_fault: Temperature::from_celsius(115),
    ot_warn: Temperature::from_celsius(95),
    power_good_on: Voltage::from_mv(1080),
    power_good_off: Voltage::from_mv(1020),
    voltage_min: Voltage::from_mv(400),
    // Single-sourced from the board descriptor so the regulator ceiling and
    // the tuner's `max_rail_voltage()` can never disagree.
    voltage_max: ubos_asic::BFMX4_HASHBOARD.max_psu_voltage,
};

/// Hashnode PSU config. The INA230 address (0x43), 2 mΩ shunt, and 30 A
/// max current all come from `BfmPsuDescriptor::DEFAULT`; only the `SIC45x`
/// voltage rail is board-specific.
const HASHNODE_PSU: BfmPsuDescriptor = BfmPsuDescriptor {
    sic: HASHNODE_SIC,
    ..BfmPsuDescriptor::DEFAULT
};

/// The Hashnode: a BM1370 hashboard on its own integrated control board,
/// with a `SIC45x`-based PSU rather than the miniminer's `TPS6287x`.
pub const HASHNODE: MachineDescriptor<BfmPsuDescriptor> = MachineDescriptor {
    model: "Braiins Forge Miner x4",
    control_board: CONTROL_BOARD_HASHNODE,
    hashboard: ubos_asic::BFMX4_HASHBOARD,
    chains: 1,
    psu: HASHNODE_PSU,
    psu_temp: &[],
};
const _: () = assert!(
    HASHNODE.chains <= MAX_CHAINS,
    "the Hashnode has more chains than MAX_CHAINS"
);

/// TPS53647 configuration for the Forge Axe 1.2 V rail. The `PMBus`
/// address is the ADDR-TRISE strap candidate (R1 16.5k / R2 24.3k),
/// pending hardware confirmation.
const FORGE_AXE_TPS: Tps53647Config = Tps53647Config {
    // Single-sourced from the board descriptor so the regulator
    // ceiling and the tuner's `max_rail_voltage()` can never disagree.
    voltage_max: ubos_asic::FORGE_AXE_HASHBOARD.max_psu_voltage,
    // The board populates three power stages.
    phases: Some(Phases::Three),
    ..Tps53647Config::DEFAULT
};

/// The Forge Axe PSU, with the TMP102 that reads it.
pub(crate) const FORGE_AXE_PSU: ForgeAxePsuDescriptor = ForgeAxePsuDescriptor {
    tps: FORGE_AXE_TPS,
    temp: &[SensorDescriptor::Tmp102(Tmp102 {
        address: 0x49,
        local: TempSource::Psu,
    })],
};

/// The Forge Axe: four BM1370 in one parallel voltage domain on an
/// integrated RP2350B control board, with a TPS53647 multiphase VRM.
pub const FORGE_AXE: MachineDescriptor<ForgeAxePsuDescriptor> = MachineDescriptor {
    model: "Forge Axe",
    control_board: CONTROL_BOARD_FORGE_AXE,
    hashboard: ubos_asic::FORGE_AXE_HASHBOARD,
    chains: 1,
    psu: FORGE_AXE_PSU,
    psu_temp: FORGE_AXE_PSU.temp,
};
const _: () = assert!(
    FORGE_AXE.chains <= MAX_CHAINS,
    "the Forge Axe has more chains than MAX_CHAINS"
);

#[cfg(test)]
#[path = "tests/machine.rs"]
mod tests;
