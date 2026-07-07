//! Machine composition: control board plus hashboard, PSU, and sensors.

use ubos_asic::HashboardDescriptor;
use ubos_psu::BmmPsuDescriptor;
use ubos_temperature::{SensorDescriptor, SensorKind};

use crate::control_board::{
    CONTROL_BOARD_BMM_PICO, CONTROL_BOARD_ESP32S3, CONTROL_BOARD_RP2040, CONTROL_BOARD_RP2350,
    CONTROL_BOARD_STM32F407, CONTROL_BOARD_STM32F439, ControlBoardDescriptor,
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
