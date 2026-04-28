//! BMM100 mining main loop.
//!
//! Brings the chip up (PSU, reset sequencing, init replay), waits on
//! [`STOP_MINING`], and tears it down (deinit replay, work stop, PSU
//! off). One linear function — error handling lives inside the
//! actor / driver methods, not here.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use ubos_logging::info;

use crate::bmm100_trace::{DEINIT, INIT};
use crate::psu::PSU;
use crate::replay::replay;
use crate::reset::ResetPin;
use crate::serial::SERIAL;

/// Rail voltage the chip runs at. Mirrors the redacted miner.
const VOLTAGE_MV: u32 = 1400;

/// Settling time held between each reset / PSU transition during
/// the bring-up sequence.
const RESET_HOLD: Duration = Duration::from_secs(5);

/// Raise once to stop the miner. The run loop awaits this between
/// init and deinit; signalling it triggers the teardown sequence.
pub static STOP_MINING: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub async fn run<R: ResetPin>(reset_pin: &mut R) {
    // -- bring-up --
    info!("miner: setting voltage to {} mV", VOLTAGE_MV);
    let _ = PSU.set_voltage(VOLTAGE_MV).await;

    info!("miner: entering reset");
    reset_pin.enter_reset();
    Timer::after(RESET_HOLD).await;

    info!("miner: enabling PSU");
    let _ = PSU.enable().await;
    Timer::after(RESET_HOLD).await;

    info!("miner: exiting reset");
    reset_pin.exit_reset();
    Timer::after(RESET_HOLD).await;

    let _ = SERIAL.reset().await;

    info!("miner: replaying init ({} steps)", INIT.len());
    let _ = replay(&SERIAL, INIT).await;

    // -- mine --
    info!("miner: mining; awaiting STOP_MINING");
    STOP_MINING.wait().await;

    // -- teardown --
    info!("miner: replaying deinit ({} steps)", DEINIT.len());
    let _ = replay(&SERIAL, DEINIT).await;

    let _ = SERIAL.stop_sending_work().await;
    let _ = SERIAL.reset().await;
    reset_pin.enter_reset();
    let _ = PSU.disable().await;
    info!("miner: stopped");
}
