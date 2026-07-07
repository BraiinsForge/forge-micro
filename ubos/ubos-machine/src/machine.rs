//! Machine composition: control board plus hashboard, PSU, and sensors.

use ubos_asic::HashboardDescriptor;
use ubos_psu::sic45x::{LoopScale, Sic45xConfig};
use ubos_psu::{BfmPsuDescriptor, BmmPsuDescriptor};
use ubos_temperature::{SensorDescriptor, SensorKind};

use crate::control_board::{
    CONTROL_BOARD_BMM_PICO, CONTROL_BOARD_ESP32S3, CONTROL_BOARD_HASHNODE, CONTROL_BOARD_RP2040,
    CONTROL_BOARD_RP2350, CONTROL_BOARD_STM32F407, CONTROL_BOARD_STM32F439, ControlBoardDescriptor,
};

/// A complete miner: the control board plus the hashboard, PSU, and
/// temperature-sensor group it drives.
///
/// Identity and capability metadata is not carried here; the REST API
/// serves it from the per-board `ubos_config::mining_info` const fn,
/// which derives it from the hashboard descriptor so a second chip model
/// reports its own values.
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
    /// The hashboard the machine drives.
    pub hashboard: HashboardDescriptor,
    /// The PSU powering the hashboard rail.
    pub psu: Psu,
    /// The hashboard temperature-sensor topology.
    pub temp: &'static [SensorDescriptor],
}

/// The BMM101 hashboard's four LM75A board sensors, IC12D..IC12A at
/// `0x49..=0x4c`, each offset by +7 °C to approximate chip junction
/// temperature. Shared by every miniminer machine and the host runner so
/// the two cannot drift.
pub const BMM101_SENSORS: [SensorDescriptor; 4] = [
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x49,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x4a,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x4b,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x4c,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
];

impl MachineDescriptor<BmmPsuDescriptor> {
    /// Compose the miniminer machine on a given control board. Every
    /// miniminer shares the same hashboard, PSU, and temperature-sensor
    /// topology; only the control board varies.
    const fn miniminer(control_board: ControlBoardDescriptor) -> Self {
        Self {
            model: "miniminer",
            control_board,
            hashboard: ubos_asic::BMM100,
            psu: BmmPsuDescriptor::DEFAULT,
            temp: &BMM101_SENSORS,
        }
    }
}

/// The miniminer on the W5500-EVB-Pico control board.
pub const MINIMINER_RP2040: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_RP2040);

/// The miniminer on the RP2350 control board.
pub const MINIMINER_RP2350: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_RP2350);

/// The miniminer on the BMM adapter control board.
pub const MINIMINER_BMM_PICO: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_BMM_PICO);

/// The miniminer on the ESP32-S3 control board.
pub const MINIMINER_ESP32S3: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_ESP32S3);

/// The miniminer on the STM32F407 control board.
pub const MINIMINER_STM32F407: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_STM32F407);

/// The miniminer on the STM32F439 control board.
pub const MINIMINER_STM32F439: MachineDescriptor<BmmPsuDescriptor> =
    MachineDescriptor::miniminer(CONTROL_BOARD_STM32F439);

/// The Hashnode's TMP461 remote diode on the ASIC die plus two TMP102 PCB
/// sensors. The TMP461 gates thermal shutdown from the real die reading;
/// the TMP102 pair drives the cooling estimate with a +15 °C PCB-to-die
/// offset and covers the safety value if the die read fails.
pub const HASHNODE_SENSORS: [SensorDescriptor; 3] = [
    SensorDescriptor {
        kind: SensorKind::Tmp461,
        address: 0x4E,
        pcb_to_die_offset_mc: 0,
        attached_chip: Some(0),
    },
    SensorDescriptor {
        kind: SensorKind::Tmp102,
        address: 0x48,
        pcb_to_die_offset_mc: 15_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Tmp102,
        address: 0x49,
        pcb_to_die_offset_mc: 15_000,
        attached_chip: None,
    },
];

/// `SIC45x` configuration for the Hashnode 4-domain 4.8 V rail.
///
/// `loop_scale` is `Scale0_25` per the `SIC45x` page VOUT-range table: the
/// 3.3 V to 5.0 V range selects register `0xE802`, whose LINEAR11 decode
/// is 0.25.
const HASHNODE_SIC: Sic45xConfig = Sic45xConfig {
    address: 0x10,
    // Idle at the 0.6 V pre-open-core floor, matching BOS PARAMS_4X1. The
    // tuner programs the 4.8 V operating voltage before enumerating the chips.
    initial_millivolts: 600,
    loop_scale: LoopScale::Scale0_25,
    // OV fault 5.0 V with headroom above the 4.8 V operating ceiling, per
    // BOS PARAMS_4X1. BOS sets its OV warn at 4.85 V, but this driver has no
    // warn field.
    vout_ov_fault_millivolts: 5000,
    vout_uv_fault_millivolts: 700,
    vin_ov_fault_millivolts: 22_000,
    vin_uv_warn_millivolts: 9_000,
    iout_oc_fault_milliamps: 25_000,
    iout_oc_warn_milliamps: 20_000,
    ot_fault_celsius: 115,
    ot_warn_celsius: 95,
    power_good_on_millivolts: 1080,
    power_good_off_millivolts: 1020,
    voltage_min_millivolts: 400,
    // Single-sourced from the board descriptor so the regulator ceiling and
    // the tuner's `max_rail_voltage()` can never disagree.
    voltage_max_millivolts: ubos_asic::HASHNODE.max_psu_voltage.mv(),
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
    hashboard: ubos_asic::HASHNODE,
    psu: HASHNODE_PSU,
    temp: &HASHNODE_SENSORS,
};
