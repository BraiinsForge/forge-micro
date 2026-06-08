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
