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

//! `SLG46826G` heartbeat-driven watchdog circuit.

use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;

const REG_VIRTUAL_INPUT: u8 = 0x7a;
const HEARTBEAT_HIGH_DURATION: Duration = Duration::from_millis(100);

pub(crate) async fn init<I, E>(i2c: &mut I, address: u8) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x00]).await
}

pub(crate) async fn heartbeat<I, E>(i2c: &mut I, address: u8) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x01]).await?;
    Timer::after(HEARTBEAT_HIGH_DURATION).await;
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x00]).await
}

#[cfg(test)]
#[path = "tests/watchdog.rs"]
mod tests;
