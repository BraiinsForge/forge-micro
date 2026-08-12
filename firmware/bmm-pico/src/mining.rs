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

//! Hashboard reset pin plus the two long-running tasks the ubos-mining backend
//! spawns: the chip-serial actor and the bring-up/teardown state machine.
//!
//! Backend architecture: `../../../docs/MINING.md`.

use embassy_rp::gpio::Output;

/// Drives the hashboard reset line through an embassy-rp GPIO.
pub struct BoardResetPin {
    pin: Output<'static>,
}

impl BoardResetPin {
    #[must_use]
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin }
    }
}

impl ubos_mining::reset::ResetPin for BoardResetPin {
    async fn enter_reset(&mut self) {
        self.pin.set_low();
    }

    async fn exit_reset(&mut self) {
        self.pin.set_high();
    }
}

ubos_mining::serial_task!(
    ubos_rp::uart::MinerUart,
    ubos_firmware::AsicSession<{ crate::POOL_SLOTS }>,
    crate::config::BOARD,
    crate::config::CHIP
);

ubos_mining::miner_task!(crate::psu::BoardPsu, BoardResetPin, crate::config::BOARD);
