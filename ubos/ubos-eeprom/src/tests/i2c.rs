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
