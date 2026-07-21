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

fn make_handle(
    bus: Mock,
    config: crate::BmmPsuDescriptor,
    terminated_value: bool,
) -> (
    BmmPsu<Mock>,
    &'static Mutex<RawMutex, State<Mock, crate::BmmPsuDescriptor>>,
) {
    let state: &'static _ = leak(Mutex::<RawMutex, _>::new(State::new(bus, config)));
    let terminated: &'static _ = leak(AtomicBool::new(terminated_value));
    (BmmPsu::new(state, terminated), state)
}

#[test]
fn terminated_short_circuits_methods() {
    let bus = Mock::new(&[]);
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let (psu, _state) = make_handle(bus, config, true);
    let result = run(psu.set_voltage(Voltage::from_mv(700)));
    assert!(matches!(result, Err(Error::Terminated)));
}

#[test]
fn dropped_set_voltage_future_does_not_corrupt_bus() {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let dac_700 = config
        .dcdc_voltage_range
        .voltage_to_dac(700)
        .expect("BUG: 700 mv");
    let dac_800 = config
        .dcdc_voltage_range
        .voltage_to_dac(800)
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
    let (psu, state) = make_handle(bus, config, false);

    // First call runs init then writes 700 mV.
    run(psu.set_voltage(Voltage::from_mv(700))).expect("BUG: first call");

    // Build a second future and let it fall out of scope
    // without polling. With the Mock's synchronous I2C, a
    // single poll would run the whole transaction; instead
    // we drop pre-poll to verify that the no-poll path leaves
    // the lock + state untouched for the next caller.
    {
        let _future = psu.set_voltage(Voltage::from_mv(800));
    }

    // Third call must complete cleanly with the bus advancing.
    run(psu.set_voltage(Voltage::from_mv(800))).expect("BUG: third call");

    let mut guard = run(state.lock());
    guard.i2c.done();
}

#[test]
fn terminate_disables_dcdc_and_short_circuits_subsequent_calls() {
    let config = crate::BmmPsuDescriptor::DEFAULT;
    let dac_700 = config
        .dcdc_voltage_range
        .voltage_to_dac(700)
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
    let (psu, state) = make_handle(bus, config, false);

    run(psu.set_voltage(Voltage::from_mv(700))).expect("BUG: set_voltage");
    run(psu.terminate());

    let post = run(psu.set_voltage(Voltage::from_mv(800)));
    assert!(matches!(post, Err(Error::Terminated)));

    let mut guard = run(state.lock());
    guard.i2c.done();
}
