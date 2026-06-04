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
    Ok(power_to_mw(
        power_register,
        current_lsb_microamps(max_current_a),
    ))
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
mod math_tests {
    use super::*;

    #[test]
    fn current_lsb_input_rail() {
        // BOS input: max_current=6 A. lsb = 6 * 1_000_000 / 32768 = 183 µA
        assert_eq!(current_lsb_microamps(6), 183);
    }

    #[test]
    fn current_lsb_output_rail() {
        // BOS output: max_current=30 A. lsb = 30 * 1_000_000 / 32768 = 915 µA
        assert_eq!(current_lsb_microamps(30), 915);
    }

    #[test]
    fn calibration_input_rail() {
        // BOS: cal = 0.00512 / (current_lsb * r_shunt)
        //         = 0.00512 / (183e-6 * 6e-3) ≈ 4663
        let cal = calibration_value(183, 6_000);
        assert!(
            (4_500..=4_800).contains(&cal),
            "cal {cal} not in expected range"
        );
    }

    #[test]
    fn power_register_to_mw() {
        // current_lsb=183 µA → power_lsb = 25 * 183 = 4575 µW per LSB.
        // power_register = 1000 → 4_575_000 µW = 4575 mW.
        assert_eq!(power_to_mw(1000, 183), 4575);
    }
}

#[cfg(test)]
mod bus_tests {
    use super::*;
    use crate::MiniminerPsuConfig;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

    fn run<F: core::future::Future>(future: F) -> F::Output {
        embassy_futures::block_on(future)
    }

    fn init_expectations(address: u8, max_current_a: u32, shunt_microohm: u32) -> [Transaction; 2] {
        let cal = calibration_value(current_lsb_microamps(max_current_a), shunt_microohm);
        let mut config_buffer = vec![REG_CONFIG];
        config_buffer.extend_from_slice(&CONFIG_VALUE.to_be_bytes());
        let mut cal_buffer = vec![REG_CALIBRATION];
        cal_buffer.extend_from_slice(&cal.to_be_bytes());
        [
            Transaction::write(address, config_buffer),
            Transaction::write(address, cal_buffer),
        ]
    }

    #[test]
    fn init_writes_config_then_calibration() {
        let config = MiniminerPsuConfig::DEFAULT;
        let expectations = init_expectations(
            config.ina_input_address,
            config.ina_input_max_current_a,
            config.ina_input_shunt_microohm,
        );
        let mut bus = Mock::new(&expectations);
        run(init(
            &mut bus,
            config.ina_input_address,
            config.ina_input_max_current_a,
            config.ina_input_shunt_microohm,
        ))
        .expect("BUG: init failed");
        bus.done();
    }

    #[test]
    fn read_power_mw_reads_power_register() {
        let address = 0x46;
        let max_current = 6;
        let expectations = [Transaction::write_read(
            address,
            vec![REG_POWER],
            // power_register = 1000 (be).
            vec![0x03, 0xe8],
        )];
        let mut bus = Mock::new(&expectations);
        let mw =
            run(read_power_mw(&mut bus, address, max_current)).expect("BUG: read_power_mw failed");
        bus.done();
        // Allow ±1 mW tolerance for integer rounding.
        assert!((4_574..=4_576).contains(&mw), "unexpected mw: {mw}");
    }

    #[test]
    fn read_bus_voltage_mv_decodes_register() {
        let address = 0x46;
        // bus_register = 1000 → 1000 * 1.25 mV = 1250 mV.
        let expectations = [Transaction::write_read(
            address,
            vec![REG_BUS_VOLTAGE],
            vec![0x03, 0xe8],
        )];
        let mut bus = Mock::new(&expectations);
        let mv =
            run(read_bus_voltage_mv(&mut bus, address)).expect("BUG: read_bus_voltage_mv failed");
        bus.done();
        assert_eq!(mv, 1250);
    }
}
