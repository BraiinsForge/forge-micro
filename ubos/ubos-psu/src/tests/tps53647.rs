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
use ubos_core::units::{Power, Voltage};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

#[test]
fn vid_codec_round_trips_the_vr120_table() {
    // Table anchors from the datasheet: 01h = 250 mV, floor + 5 mV
    // per step, FFh = 1520 mV.
    assert_eq!(encode_vid(Voltage::from_mv(250)), 0x01);
    assert_eq!(encode_vid(Voltage::from_mv(1200)), 0xBF);
    assert_eq!(encode_vid(Voltage::from_mv(1520)), 0xFF);
    assert_eq!(Voltage::from(RawVid(0x01)), Voltage::from_mv(250));
    assert_eq!(Voltage::from(RawVid(0xBF)), Voltage::from_mv(1200));
    assert_eq!(Voltage::from(RawVid(0xFF)), Voltage::from_mv(1520));
    // VID 0 means "off", not a voltage.
    assert_eq!(Voltage::from(RawVid(0x00)), Voltage::from_mv(0));
    // Off-grid values round to the nearest 5 mV step.
    assert_eq!(encode_vid(Voltage::from_mv(1202)), 0xBF);
    assert_eq!(encode_vid(Voltage::from_mv(1203)), 0xC0);
}

#[test]
fn status_decode_maps_the_pmbus_fault_bits() {
    let flags = decode_status(1 << 5 | 1 << 2);
    assert_eq!(
        flags,
        crate::StatusFlags {
            overvoltage: true,
            thermal_shutdown: true,
            current_limit: false,
            undervoltage: false,
        }
    );
    assert_eq!(decode_status(0), crate::StatusFlags::default());
}

/// Build the init wire sequence for a `Tps53647Config`.
fn init_transactions(config: &Tps53647Config) -> Vec<Transaction> {
    let vout_max = u16::from(encode_vid(config.voltage_max)).to_le_bytes();
    let ot_fault = crate::sic45x::encode_temp(config.ot_fault).to_le_bytes();
    let ot_warn = crate::sic45x::encode_temp(config.ot_warn).to_le_bytes();
    let vin_ov = encode_vin_ov(config.vin_ov_fault).to_le_bytes();
    let vout_cmd = u16::from(encode_vid(config.initial_voltage)).to_le_bytes();
    let mut transactions = vec![Transaction::write_read(
        config.address,
        vec![VOUT_MODE],
        vec![VOUT_MODE_VID_VR120],
    )];
    if let Some(phases) = config.phases {
        transactions.extend(phases_transactions(
            config.address,
            phases,
            phases.phase_num(),
        ));
    }
    transactions.extend([
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
    ]);
    transactions
}

/// The phase-count write and its readback, answered with `readback`.
fn phases_transactions(address: u8, phases: Phases, readback: u8) -> [Transaction; 2] {
    [
        Transaction::write(address, vec![MFR_SPECIFIC_20, phases.phase_num()]),
        Transaction::write_read(address, vec![MFR_SPECIFIC_20], vec![readback]),
    ]
}

#[test]
fn init_programs_limits_and_initial_voltage() {
    let config = Tps53647Config::DEFAULT;
    let mut bus = Mock::new(&init_transactions(&config));
    run(init(&mut bus, &config)).expect("init should succeed");
    bus.done();
}

#[test]
fn init_programs_the_phase_count() {
    let config = Tps53647Config {
        phases: Some(Phases::Three),
        ..Tps53647Config::DEFAULT
    };
    let mut bus = Mock::new(&init_transactions(&config));
    run(init(&mut bus, &config)).expect("init should succeed");
    bus.done();
}

#[test]
fn init_rejects_a_phase_count_the_pinstraps_do_not_populate() {
    let config = Tps53647Config {
        phases: Some(Phases::Three),
        ..Tps53647Config::DEFAULT
    };
    // 0x01 = two phases: the device kept its pinstrapped count.
    let mut transactions = vec![Transaction::write_read(
        config.address,
        vec![VOUT_MODE],
        vec![VOUT_MODE_VID_VR120],
    )];
    transactions.extend(phases_transactions(config.address, Phases::Three, 0x01));
    let mut bus = Mock::new(&transactions);
    let result = run(init(&mut bus, &config));
    assert!(matches!(result, Err(Error::OutOfRange)));
    bus.done();
}

#[test]
fn init_rejects_a_non_vr120_vout_mode() {
    let config = Tps53647Config::DEFAULT;
    // 0x22 = VID mode but the VR12.5 table: the strap disagrees with
    // the driver's VID encoding, so no voltage may be commanded.
    let mut bus = Mock::new(&[Transaction::write_read(
        config.address,
        vec![VOUT_MODE],
        vec![0x22],
    )]);
    let result = run(init(&mut bus, &config));
    assert!(matches!(result, Err(Error::OutOfRange)));
    bus.done();
}

#[test]
fn enable_sets_the_operation_on_bit_and_clears_faults() {
    let config = Tps53647Config::DEFAULT;
    let mut bus = Mock::new(&[
        Transaction::write_read(config.address, vec![OPERATION], vec![0x00]),
        Transaction::write(config.address, vec![OPERATION, 0x80]),
        Transaction::write(config.address, vec![CLEAR_FAULTS]),
    ]);
    run(enable(&mut bus, &config)).expect("enable should succeed");
    bus.done();
}

#[test]
fn disable_clears_the_operation_on_bit() {
    let config = Tps53647Config::DEFAULT;
    let mut bus = Mock::new(&[
        Transaction::write_read(config.address, vec![OPERATION], vec![0x80]),
        Transaction::write(config.address, vec![OPERATION, 0x00]),
    ]);
    run(disable(&mut bus, &config)).expect("disable should succeed");
    bus.done();
}

#[test]
fn set_voltage_writes_the_vid_and_rejects_out_of_range() {
    let config = Tps53647Config::DEFAULT;
    let mut bus = Mock::new(&[Transaction::write(
        config.address,
        vec![VOUT_COMMAND, 0xBF, 0x00],
    )]);
    run(set_voltage(&mut bus, &config, Voltage::from_mv(1200))).expect("in-range voltage");
    bus.done();

    let mut bus = Mock::new(&[]);
    assert!(matches!(
        run(set_voltage(&mut bus, &config, Voltage::from_mv(249))),
        Err(Error::OutOfRange)
    ));
    assert!(matches!(
        run(set_voltage(&mut bus, &config, Voltage::from_mv(1521))),
        Err(Error::OutOfRange)
    ));
    bus.done();
}

#[test]
fn read_vout_decodes_the_vid_word() {
    let config = Tps53647Config::DEFAULT;
    let mut bus = Mock::new(&[Transaction::write_read(
        config.address,
        vec![READ_VOUT],
        vec![0xBF, 0x00],
    )]);
    let voltage = run(read_vout(&mut bus, &config)).expect("read should succeed");
    assert_eq!(voltage, Voltage::from_mv(1200));
    bus.done();
}

#[test]
fn read_pout_decodes_linear11_to_milliwatts() {
    let config = Tps53647Config::DEFAULT;
    // linear11: exponent 0 (upper 5 bits zero), mantissa 100 -> 100 W.
    let raw = 100u16.to_le_bytes();
    let mut bus = Mock::new(&[Transaction::write_read(
        config.address,
        vec![READ_POUT],
        vec![raw[0], raw[1]],
    )]);
    let power = run(read_pout(&mut bus, &config)).expect("read should succeed");
    assert_eq!(power, Power::from_mw(100_000));
    bus.done();
}

#[test]
fn read_pin_decodes_linear11_to_milliwatts() {
    let config = Tps53647Config::DEFAULT;
    // linear11: exponent -2 (0x1E in the top 5 bits), mantissa 402
    // -> 100.5 W.
    let raw = ((0x1Eu16 << 11) | 0x192).to_le_bytes();
    let mut bus = Mock::new(&[Transaction::write_read(
        config.address,
        vec![READ_PIN],
        vec![raw[0], raw[1]],
    )]);
    let power = run(read_pin(&mut bus, &config)).expect("read should succeed");
    assert_eq!(power, Power::from_mw(100_500));
    bus.done();
}

#[test]
fn vin_ov_limit_encodes_whole_volts_rounded() {
    assert_eq!(encode_vin_ov(Voltage::from_mv(14_000)), 14);
    assert_eq!(encode_vin_ov(Voltage::from_mv(14_499)), 14);
    assert_eq!(encode_vin_ov(Voltage::from_mv(14_500)), 15);
}
