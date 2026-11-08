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
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

fn leak<T: 'static>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn make_handle(bus: Mock) -> (ForgeAxePsu<Mock>, Mock) {
    let bus_handle = bus.clone();
    let shared: &'static _ = leak(ForgeAxeShared::new(bus, ForgeAxePsuDescriptor::DEFAULT));
    (ForgeAxePsu::new(shared), bus_handle)
}

/// The init wire sequence for `ForgeAxePsuDescriptor::DEFAULT`, mirroring
/// `tests/tps53647.rs::init_transactions`.
fn init_transactions() -> Vec<Transaction> {
    use crate::tps53647::{
        CLEAR_FAULTS, OT_FAULT_LIMIT, OT_WARN_LIMIT, Tps53647Config, VIN_OV_FAULT_LIMIT,
        VOUT_COMMAND, VOUT_MAX, VOUT_MODE, VOUT_MODE_VID_VR120, encode_vid,
    };
    let config = Tps53647Config::DEFAULT;
    let vout_max = u16::from(encode_vid(config.voltage_max)).to_le_bytes();
    let ot_fault = crate::sic45x::encode_temp(config.ot_fault).to_le_bytes();
    let ot_warn = crate::sic45x::encode_temp(config.ot_warn).to_le_bytes();
    let vin_ov = crate::tps53647::encode_vin_ov(config.vin_ov_fault).to_le_bytes();
    let vout_cmd = u16::from(encode_vid(config.initial_voltage)).to_le_bytes();
    vec![
        Transaction::write_read(config.address, vec![VOUT_MODE], vec![VOUT_MODE_VID_VR120]),
        Transaction::write(config.address, vec![VOUT_MAX, vout_max[0], vout_max[1]]),
        Transaction::write(
            config.address,
            vec![OT_FAULT_LIMIT, ot_fault[0], ot_fault[1]],
        ),
        Transaction::write(config.address, vec![OT_WARN_LIMIT, ot_warn[0], ot_warn[1]]),
        Transaction::write(
            config.address,
            vec![VIN_OV_FAULT_LIMIT, vin_ov[0], vin_ov[1]],
        ),
        Transaction::write(config.address, vec![VOUT_COMMAND, vout_cmd[0], vout_cmd[1]]),
        Transaction::write(config.address, vec![CLEAR_FAULTS]),
    ]
}

#[test]
fn first_call_runs_init_then_the_operation() {
    use crate::tps53647::{READ_POUT, Tps53647Config};
    let address = Tps53647Config::DEFAULT.address;
    // linear11 exponent 0, mantissa 50 -> 50 W.
    let raw = 50u16.to_le_bytes();
    let mut transactions = init_transactions();
    transactions.push(Transaction::write_read(
        address,
        vec![READ_POUT],
        vec![raw[0], raw[1]],
    ));
    // A second read must replay the cached init, not the wire
    // sequence: the mock holds exactly one extra read transaction.
    transactions.push(Transaction::write_read(
        address,
        vec![READ_POUT],
        vec![raw[0], raw[1]],
    ));
    let (psu, mut bus) = make_handle(Mock::new(&transactions));
    let power = run(psu.read_output_power()).expect("read should succeed");
    assert_eq!(power.mw(), 50_000);
    let power = run(psu.read_output_power()).expect("read should succeed");
    assert_eq!(power.mw(), 50_000);
    bus.done();
}

#[test]
fn terminated_psu_rejects_operations() {
    let (psu, mut bus) = make_handle(Mock::new(&[]));
    run(psu.terminate());
    assert!(matches!(run(psu.enable()), Err(Error::Terminated)));
    assert!(matches!(
        run(psu.set_voltage(ubos_core::units::Voltage::from_mv(1200))),
        Err(Error::Terminated)
    ));
    assert!(matches!(run(psu.read_status()), Err(Error::Terminated)));
    assert!(matches!(
        run(psu.read_input_power()),
        Err(Error::Terminated)
    ));
    bus.done();
}

#[test]
fn terminate_disables_an_initialized_rail_once() {
    use crate::tps53647::{OPERATION, READ_VOUT, Tps53647Config};
    let address = Tps53647Config::DEFAULT.address;
    let vid = 0xBFu8;
    let mut transactions = init_transactions();
    transactions.extend([
        Transaction::write_read(address, vec![READ_VOUT], vec![vid, 0x00]),
        Transaction::write_read(address, vec![OPERATION], vec![0x80]),
        Transaction::write(address, vec![OPERATION, 0x00]),
    ]);
    let (psu, mut bus) = make_handle(Mock::new(&transactions));
    let voltage = run(psu.read_output_voltage()).expect("read should succeed");
    assert_eq!(voltage.mv(), 1200);
    run(psu.terminate());
    // Second terminate is a no-op; the mock would fail on extra I2C.
    run(psu.terminate());
    bus.done();
}

#[test]
fn voltage_source_names_the_part() {
    let (psu, _bus) = make_handle(Mock::new(&[]));
    assert_eq!(psu.voltage_source(), Some("tps53647"));
    assert_eq!(psu.secondary_voltage_source(), None);
    assert!(run(psu.read_secondary_voltage()).is_none());
}
