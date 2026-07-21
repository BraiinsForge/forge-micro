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
