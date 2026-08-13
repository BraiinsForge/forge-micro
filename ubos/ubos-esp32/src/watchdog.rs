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

//! The ESP32-S3 timer-group watchdog behind the shared feed loop's trait.

use esp_hal::peripherals::TIMG1;
use esp_hal::time::Duration;
use esp_hal::timer::timg::{MwdtStage, Wdt};

/// The TIMG1 watchdog wrapped for `ubos_core::watchdog::run_watchdog`. It
/// stores the timeout so the board's `config::Watchdog::TIMEOUT` does not have
/// to reach this crate.
pub struct BoardWatchdog {
    wdt: Wdt<TIMG1<'static>>,
    timeout: Duration,
}

impl BoardWatchdog {
    #[must_use]
    pub fn new(wdt: Wdt<TIMG1<'static>>, timeout: Duration) -> Self {
        Self { wdt, timeout }
    }
}

impl ubos_core::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.wdt.enable();
        self.wdt.set_timeout(MwdtStage::Stage0, self.timeout);
    }

    fn feed(&mut self) {
        self.wdt.feed();
    }
}
