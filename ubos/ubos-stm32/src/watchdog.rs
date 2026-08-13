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

//! The STM32 independent watchdog behind the shared feed loop's trait.

use embassy_stm32::Peri;
use embassy_stm32::peripherals::IWDG;
use embassy_stm32::wdg::IndependentWatchdog;

/// The STM32 IWDG wrapped for `ubos_core::watchdog::run_watchdog`. It builds
/// the `IndependentWatchdog` internally so the board's
/// `config::Watchdog::TIMEOUT_US` does not have to reach this crate.
pub struct BoardWatchdog(IndependentWatchdog<'static, IWDG>);

impl BoardWatchdog {
    #[must_use]
    pub fn new(iwdg: Peri<'static, IWDG>, timeout_us: u32) -> Self {
        Self(IndependentWatchdog::new(iwdg, timeout_us))
    }
}

impl ubos_core::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.unleash();
    }

    fn feed(&mut self) {
        self.0.pet();
    }
}
