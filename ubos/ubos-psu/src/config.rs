//! Runtime configuration for `BmmPsu`.

use embassy_time::Duration;
use ubos_core::units::Voltage;

use crate::tps6287x::{SoftStartRampTime, VoltageRampSpeed, VoltageRange};

#[derive(Clone, Copy, Debug)]
pub struct BmmPsuDescriptor {
    pub dcdc_address: u8,
    pub dcdc_initial: Voltage,
    pub dcdc_voltage_range: VoltageRange,
    pub dcdc_ramp_speed: VoltageRampSpeed,
    pub dcdc_soft_start: SoftStartRampTime,
    pub watchdog_address: u8,
    pub watchdog_interval: Duration,
    pub ina_input_address: u8,
    pub ina_output_address: u8,
    pub ina_input_max_current_a: u32,
    pub ina_output_max_current_a: u32,
    pub ina_input_shunt_microohm: u32,
    pub ina_output_shunt_microohm: u32,
}

impl BmmPsuDescriptor {
    /// Defaults that match the BOS `BMM100_PSU_DESCRIPTOR` miniminer
    /// constants. Despite the name, that PSU config is shared verbatim
    /// between the BMM100 and BMM101 boards in `bos-main`.
    pub const DEFAULT: Self = Self {
        dcdc_address: 0x40,
        dcdc_initial: Voltage::from_mv(750),
        dcdc_voltage_range: VoltageRange::VRange2,
        dcdc_ramp_speed: VoltageRampSpeed::VRamp1_25,
        dcdc_soft_start: SoftStartRampTime::SSTime1,
        watchdog_address: 0x08,
        watchdog_interval: Duration::from_secs(5),
        ina_input_address: 0x46,
        ina_output_address: 0x43,
        ina_input_max_current_a: 6,
        ina_output_max_current_a: 30,
        ina_input_shunt_microohm: 6_000,
        ina_output_shunt_microohm: 1_960,
    };
}
