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

//! One-shot reboot request signal shared between the HTTP API, the config
//! task, and the per-board reset task. The API calls [`request_reboot`]
//! from the restart endpoint, and the config task calls it after a factory
//! reset has been persisted; the board's main spawns a task that blocks on
//! [`wait_for_reboot_request`] and then performs the MCU reset. Lives in
//! `ubos-core` so the API crate, the config crate, and each board firmware
//! crate can depend on it without introducing a cycle.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};

/// Signal that carries the reboot request from the API to the board's
/// reset task. Private; callers use the public wrapper functions below.
static REBOOT_REQUESTED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Delay between a reboot request and the MCU reset, giving the HTTP
/// response that triggered the reboot time to reach the wire before the
/// socket tears down.
const REBOOT_FLUSH_DELAY: Duration = Duration::from_millis(500);

/// Signal that the MCU should be rebooted. Sends a one-shot wake to the
/// board's reset task, which will then perform the actual reset.
pub fn request_reboot() {
    REBOOT_REQUESTED.signal(());
}

/// Suspend until a reboot has been requested. Resolves once
/// [`request_reboot`] has been called, then waits a short flush delay so
/// the HTTP response that triggered the reboot reaches the wire before
/// the caller resets the MCU. The caller then performs the MCU reset and
/// never returns.
pub async fn wait_for_reboot_request() {
    REBOOT_REQUESTED.wait().await;
    Timer::after(REBOOT_FLUSH_DELAY).await;
}

/// Task shell around [`wait_for_reboot_request`], which resets the MCU
/// once a reboot has been requested through the HTTP API.
///
/// Takes the reset itself as an expression: there is no architectural
/// self-reset shared across these targets, so each board supplies its
/// own diverging call.
#[macro_export]
macro_rules! reset_task {
    ($reset:expr) => {
        #[embassy_executor::task]
        async fn reset_task() -> ! {
            ubos_core::reboot::wait_for_reboot_request().await;
            $reset
        }
    };
}
