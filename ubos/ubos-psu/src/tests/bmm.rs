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

use super::*;
use crate::state::test_support;
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

fn leak<T: 'static>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn make_handle(bus: Mock, config: crate::BmmPsuDescriptor) -> (BmmPsu<Mock>, Mock) {
    let bus_handle = bus.clone();
    let shared: &'static _ = leak(BmmShared::new(bus, config));
    (BmmPsu::new(shared), bus_handle)
}

#[test]
fn terminated_short_circuits_methods() {
    let bus = Mock::new(&[]);
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let (psu, mut bus) = make_handle(bus, config);
    run(psu.terminate());
    let result = run(psu.set_voltage(Voltage::from_mv(700)));
    assert!(matches!(result, Err(Error::Terminated)));
    bus.done();
}

#[test]
fn dropped_set_voltage_future_does_not_corrupt_bus() {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let dac_700 = config
        .dcdc_voltage_range
        .voltage_to_dac(Voltage::from_mv(700))
        .expect("BUG: 700 mv");
    let dac_800 = config
        .dcdc_voltage_range
        .voltage_to_dac(Voltage::from_mv(800))
        .expect("BUG: 800 mv");

    let mut transactions = test_support::init_transactions(&config);
    transactions.push(Transaction::write(
        config.dcdc_address,
        tps6287x::encode_vset(dac_700).to_vec(),
    ));
    transactions.push(Transaction::write(
        config.dcdc_address,
        tps6287x::encode_vset(dac_800).to_vec(),
    ));

    let bus = Mock::new(&transactions);
    let (psu, mut bus) = make_handle(bus, config);

    // First call runs init then writes 700 mV.
    run(psu.set_voltage(Voltage::from_mv(700))).expect("BUG: first call");

    // Build a second future and drop it before polling: with the
    // Mock's synchronous I2C, a single poll would run the whole
    // transaction. Dropping pre-poll instead proves the no-poll path
    // leaves the lock and state untouched for the next caller.
    {
        let _future = psu.set_voltage(Voltage::from_mv(800));
    }

    // Third call must complete cleanly with the bus advancing.
    run(psu.set_voltage(Voltage::from_mv(800))).expect("BUG: third call");

    bus.done();
}

#[test]
fn terminate_disables_dcdc_and_short_circuits_subsequent_calls() {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let dac_700 = config
        .dcdc_voltage_range
        .voltage_to_dac(Voltage::from_mv(700))
        .expect("BUG: 700 mv");

    let mut transactions = test_support::init_transactions(&config);
    // set_voltage(700) forces init.
    transactions.push(Transaction::write(
        config.dcdc_address,
        tps6287x::encode_vset(dac_700).to_vec(),
    ));
    // terminate disables the DC/DC.
    transactions.push(Transaction::write(
        config.dcdc_address,
        tps6287x::encode_control1(false, config.dcdc_ramp_speed).to_vec(),
    ));

    let bus = Mock::new(&transactions);
    let (psu, mut bus) = make_handle(bus, config);

    run(psu.set_voltage(Voltage::from_mv(700))).expect("BUG: set_voltage");
    run(psu.terminate());

    let post = run(psu.set_voltage(Voltage::from_mv(800)));
    assert!(matches!(post, Err(Error::Terminated)));

    bus.done();
}

/// Read the status of a converter whose CONTROL1 reads `control1` and
/// whose STATUS reports undervoltage.
fn undervoltage_status(control1: u8) -> crate::StatusFlags {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let mut transactions = test_support::init_transactions(&config);
    transactions.push(Transaction::write_read(
        config.dcdc_address,
        vec![tps6287x::REG_CONTROL1],
        vec![control1],
    ));
    transactions.push(Transaction::write_read(
        config.dcdc_address,
        vec![tps6287x::REG_STATUS],
        vec![0b0000_0010],
    ));
    let (psu, mut bus) = make_handle(Mock::new(&transactions), config);
    let flags = run(psu.read_status()).expect("BUG: read_status failed");
    bus.done();
    flags
}

#[test]
fn undervoltage_is_cleared_while_the_converter_is_stopped() {
    assert!(!undervoltage_status(0).undervoltage);
}

#[test]
fn undervoltage_is_reported_while_the_converter_switches() {
    assert!(undervoltage_status(tps6287x::CONTROL1_SWEN).undervoltage);
}

#[test]
fn undervoltage_latched_before_a_disable_is_reported_once() {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let control1 = |value: u8| {
        Transaction::write_read(
            config.dcdc_address,
            vec![tps6287x::REG_CONTROL1],
            vec![value],
        )
    };
    let status = |value: u8| {
        Transaction::write_read(config.dcdc_address, vec![tps6287x::REG_STATUS], vec![value])
    };
    let mut transactions = test_support::init_transactions(&config);
    // The enable runs to its end and clears what the ramp latched.
    transactions.push(Transaction::write(
        config.dcdc_address,
        tps6287x::encode_control1(true, config.dcdc_ramp_speed).to_vec(),
    ));
    transactions.push(status(0b0000_0010));
    // The disable finds the converter switching with undervoltage latched.
    transactions.push(control1(tps6287x::CONTROL1_SWEN));
    transactions.push(status(0b0000_0010));
    transactions.push(Transaction::write(
        config.dcdc_address,
        tps6287x::encode_control1(false, config.dcdc_ramp_speed).to_vec(),
    ));
    // Both status reads find the converter stopped and reporting that as
    // undervoltage.
    for _ in 0..2 {
        transactions.push(control1(0));
        transactions.push(status(0b0000_0010));
    }
    let (psu, mut bus) = make_handle(Mock::new(&transactions), config);

    run(psu.enable()).expect("BUG: enable failed");
    run(psu.disable()).expect("BUG: disable failed");
    let first = run(psu.read_status()).expect("BUG: first read_status failed");
    let second = run(psu.read_status()).expect("BUG: second read_status failed");

    assert!(first.undervoltage);
    assert!(!second.undervoltage);
    bus.done();
}

#[test]
fn undervoltage_of_a_ramp_cut_short_is_not_latched_by_the_disable() {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let switch = |on: bool| {
        Transaction::write(
            config.dcdc_address,
            tps6287x::encode_control1(on, config.dcdc_ramp_speed).to_vec(),
        )
    };
    let mut transactions = test_support::init_transactions(&config);
    transactions.push(switch(false));
    // The enable is dropped in its soft-start wait, before its status read.
    transactions.push(switch(true));
    // The disable stops the converter and reads no flags.
    transactions.push(switch(false));
    // The status read finds the converter stopped, with the undervoltage
    // of the ramp still latched.
    transactions.push(Transaction::write_read(
        config.dcdc_address,
        vec![tps6287x::REG_CONTROL1],
        vec![0],
    ));
    transactions.push(Transaction::write_read(
        config.dcdc_address,
        vec![tps6287x::REG_STATUS],
        vec![0b0000_0010],
    ));
    let (psu, mut bus) = make_handle(Mock::new(&transactions), config);

    run(psu.disable()).expect("BUG: first disable failed");
    assert!(embassy_futures::poll_once(psu.enable()).is_pending());
    run(psu.disable()).expect("BUG: second disable failed");
    let flags = run(psu.read_status()).expect("BUG: read_status failed");

    assert!(!flags.undervoltage);
    bus.done();
}
