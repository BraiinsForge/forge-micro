use super::*;
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

#[test]
fn init_writes_low_to_virtual_input() {
    let address = 0x08;
    let expectations = [Transaction::write(address, vec![REG_VIRTUAL_INPUT, 0x00])];
    let mut bus = Mock::new(&expectations);
    run(init(&mut bus, address)).expect("BUG: init failed");
    bus.done();
}

#[test]
fn heartbeat_pulses_high_then_low() {
    let address = 0x08;
    let expectations = [
        Transaction::write(address, vec![REG_VIRTUAL_INPUT, 0x01]),
        Transaction::write(address, vec![REG_VIRTUAL_INPUT, 0x00]),
    ];
    let mut bus = Mock::new(&expectations);
    run(heartbeat(&mut bus, address)).expect("BUG: heartbeat failed");
    bus.done();
}
