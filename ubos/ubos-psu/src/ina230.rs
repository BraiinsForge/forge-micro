//! `INA230` high-side current/power monitor.
//!
//! Calibration math operates in integer microamps and microohms:
//! `current_lsb_microamps = max_current_a * 1_000_000 / 32_768`
//! `cal_value = 5_120_000_000 / (current_lsb_microamps * shunt_microohm)`
//! `power_mw = power_register * 25 * current_lsb_microamps / 1_000_000`

use embedded_hal_async::i2c::I2c;

const REG_CONFIG: u8 = 0x00;
const REG_BUS_VOLTAGE: u8 = 0x02;
const REG_POWER: u8 = 0x03;
const REG_CALIBRATION: u8 = 0x05;

/// Bus-voltage LSB in microvolts. INA230 reports the rail voltage in
/// 1.25 mV steps (datasheet §7.6.2.2).
const BUS_VOLTAGE_LSB_MICROVOLTS: u32 = 1_250;

/// CONFIG: averaging=Avg1, `VBus` CT=1.1ms, Shunt CT=1.1ms, Mode=continuous
/// shunt+bus. Bits laid out per datasheet Table 6-2.
const CONFIG_VALUE: u16 = 0b0100_0001_0010_0111;

pub(crate) const fn current_lsb_microamps(max_current_a: u32) -> u32 {
    max_current_a * 1_000_000 / 32_768
}

pub(crate) const fn calibration_value(current_lsb_microamps: u32, shunt_microohm: u32) -> u16 {
    // 5.12e-3 / (lsb_µA * 1e-6 * shunt_µΩ * 1e-6)
    //   = 5.12e9 / (lsb_µA * shunt_µΩ)
    let numerator: u64 = 5_120_000_000;
    let denominator: u64 = current_lsb_microamps as u64 * shunt_microohm as u64;
    let cal = numerator / denominator;
    if cal > u16::MAX as u64 {
        u16::MAX
    } else {
        cal as u16
    }
}

pub(crate) const fn power_to_mw(power_register: u16, current_lsb_microamps: u32) -> u32 {
    // power_lsb_microwatts = 25 * current_lsb_microamps (datasheet)
    // power_milliwatts = power_register * power_lsb_microwatts / 1_000
    let microwatts = power_register as u64 * 25 * current_lsb_microamps as u64;
    (microwatts / 1_000) as u32
}

pub(crate) async fn init<I, E>(
    i2c: &mut I,
    address: u8,
    max_current_a: u32,
    shunt_microohm: u32,
) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    let mut config_buffer = [0u8; 3];
    config_buffer[0] = REG_CONFIG;
    config_buffer[1..].copy_from_slice(&CONFIG_VALUE.to_be_bytes());
    i2c.write(address, &config_buffer).await?;

    let cal = calibration_value(current_lsb_microamps(max_current_a), shunt_microohm);
    let mut cal_buffer = [0u8; 3];
    cal_buffer[0] = REG_CALIBRATION;
    cal_buffer[1..].copy_from_slice(&cal.to_be_bytes());
    i2c.write(address, &cal_buffer).await
}

pub(crate) async fn read_power_mw<I, E>(
    i2c: &mut I,
    address: u8,
    max_current_a: u32,
) -> Result<u32, E>
where
    I: I2c<Error = E>,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(address, &[REG_POWER], &mut buffer).await?;
    let power_register = u16::from_be_bytes(buffer);
    let milliwatts = power_to_mw(power_register, current_lsb_microamps(max_current_a));
    #[cfg(feature = "defmt")]
    ubos_logging::trace!(
        "INA230 power @{:x}: raw=[{:x} {:x}] reg={:x} -> {} mW",
        address,
        buffer[0],
        buffer[1],
        power_register,
        milliwatts,
    );
    Ok(milliwatts)
}

/// Read the bus-voltage register and return millivolts.
///
/// The register is unsigned 16-bit with a 1.25 mV LSB, so the value
/// fits in `u32` for any rail this driver targets.
pub(crate) async fn read_bus_voltage_mv<I, E>(i2c: &mut I, address: u8) -> Result<u32, E>
where
    I: I2c<Error = E>,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(address, &[REG_BUS_VOLTAGE], &mut buffer)
        .await?;
    let register = u16::from_be_bytes(buffer);
    Ok((u32::from(register) * BUS_VOLTAGE_LSB_MICROVOLTS) / 1_000)
}

#[cfg(test)]
#[path = "tests/ina230.rs"]
mod tests;
