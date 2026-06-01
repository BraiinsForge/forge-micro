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
    let mw = run(read_power_mw(&mut bus, address, max_current)).expect("BUG: read_power_mw failed");
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
    let mv = run(read_bus_voltage_mv(&mut bus, address)).expect("BUG: read_bus_voltage_mv failed");
    bus.done();
    assert_eq!(mv, 1250);
}
