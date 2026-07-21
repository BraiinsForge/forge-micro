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

//! Periodic hardware watchdog feeder. A cooperative task-level watchdog
//! is out of scope for these example boards.

use embassy_time::Timer;

/// Hardware watchdog abstraction for the shared feed loop.
pub trait Watchdog {
    /// Start or enable the watchdog timer.
    fn init(&mut self);
    /// Feed the watchdog to prevent reset.
    fn feed(&mut self);
}

/// Periodically feed a hardware watchdog. Runs forever.
pub async fn run_watchdog<W: Watchdog>(mut wdt: W) -> ! {
    wdt.init();
    loop {
        wdt.feed();
        Timer::after(crate::consts::Watchdog::FEED_PERIOD).await;
    }
}
