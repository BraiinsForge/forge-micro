use super::*;
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

#[test]
fn reads_the_whole_chip_in_one_transaction() {
    let expected = [0x42u8; crate::format::EEPROM_LEN];
    let mut bus = Mock::new(&[Transaction::write_read(0x50, vec![0], expected.to_vec())]);

    let mut buf = [0u8; crate::format::EEPROM_LEN];
    let mut eeprom = I2cEeprom::new(0x50, &mut bus);
    run(HashboardEeprom::read(&mut eeprom, &mut buf)).expect("mock read must succeed");

    assert_eq!(buf, expected);
    bus.done();
}

#[test]
fn uses_the_configured_address() {
    let mut bus = Mock::new(&[Transaction::write_read(
        0x51,
        vec![0],
        vec![0u8; crate::format::EEPROM_LEN],
    )]);

    let mut buf = [0u8; crate::format::EEPROM_LEN];
    let mut eeprom = I2cEeprom::new(0x51, &mut bus);
    run(HashboardEeprom::read(&mut eeprom, &mut buf)).expect("mock read must succeed");

    bus.done();
}
