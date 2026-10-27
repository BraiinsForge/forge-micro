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
//!
//! The part holds the board in reset unless it sees the virtual input
//! toggle. [`heartbeat`] is what stops that, so a caller that misses the
//! interval resets the board.
//!
//! Both functions take the bus as `&mut I` rather than hanging off a
//! handle, for the reason `ina230.rs` gives: `I` is a generic parameter
//! that no inherent `impl` here can name, and the caller has already
//! locked the `State` it drives this part and the converter through.

use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;

#[cfg(test)]
#[path = "tests/watchdog.rs"]
mod tests;

const REG_VIRTUAL_INPUT: u8 = 0x7a;
const HEARTBEAT_HIGH_DURATION: Duration = Duration::from_millis(100);

/// Drive the watchdog's virtual input low, its resting level.
///
/// Run once before the first [`heartbeat`], so the first pulse is a
/// rising edge whatever the input was left at by a warm reset.
///
/// # Errors
///
/// Returns the bus error when the write fails.
pub(crate) async fn init<I, E>(i2c: &mut I, address: u8) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x00]).await
}

/// Pulse the watchdog's virtual input high, then return it low.
///
/// This is what keeps the board out of reset. The high level is held for
/// [`HEARTBEAT_HIGH_DURATION`] so the part registers an edge, and the
/// function awaits that hold, so a caller must budget for it inside its
/// own interval.
///
/// `bmm::run_watchdog` is the only caller that meets the interval. Any
/// other caller that borrows this pattern owns the same obligation.
///
/// # Errors
///
/// Returns the bus error when either write fails. A failed heartbeat
/// leaves the input wherever the failure left it, and the next call
/// starts again from the low write.
pub(crate) async fn heartbeat<I, E>(i2c: &mut I, address: u8) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x01]).await?;
    Timer::after(HEARTBEAT_HIGH_DURATION).await;
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x00]).await
}
