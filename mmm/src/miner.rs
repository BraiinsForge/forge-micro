//! BMM100 mining main loop.
//!
//! Each iteration brings the chip up (PSU, reset sequencing, init
//! replay), waits on [`STOP_MINING`], and tears it down (deinit replay,
//! work stop, PSU off). On a fatal init failure (chip goes silent at
//! a new line rate, etc.) the iteration cuts the rail, sleeps, and
//! starts over so a transient bring-up failure doesn't leave the chip
//! powered without supervision.

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use ubos_logging::{error, info, Debug2Format};
use ubos_psu::Psu;

/// Mirrors the chip rail state for downstream consumers (host
/// fan controller etc.). `true` between PSU enable and PSU disable
/// in `run`; `false` at startup and after teardown. Replaces the
/// legacy `ubos_backend::MINER_IS_RUNNING`.
pub static MINER_IS_RUNNING: Signal<CriticalSectionRawMutex, bool> = Signal::new();

use crate::bmm100_trace::{DEINIT, INIT};
use crate::replay::replay;
use crate::reset::ResetPin;
use crate::serial::SERIAL;

/// Rail voltage the chip runs at. Mirrors the redacted miner.
const VOLTAGE_MV: u32 = 1400;

/// Settling time held between each reset / PSU transition during
/// the bring-up sequence.
const RESET_HOLD: Duration = Duration::from_secs(5);

/// Pause between a failed bring-up and the next attempt. Long enough
/// for the chip's rail to fully discharge and any transient cable /
/// board glitch to clear.
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Raise once to stop the miner. The run loop awaits this between init
/// and deinit; signalling it triggers the teardown sequence and the
/// outer loop returns.
pub static STOP_MINING: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub async fn run<P, R>(psu: &P, reset_pin: &mut R)
where
    P: Psu,
    R: ResetPin,
{
    // Publish the initial state so a fan task that started before us
    // sees `false` instead of waiting forever for the first edge.
    MINER_IS_RUNNING.signal(false);

    loop {
        // -- bring-up --
        info!("miner: setting voltage to {} mV", VOLTAGE_MV);
        let _ = psu.set_voltage(VOLTAGE_MV).await;

        info!("miner: entering reset");
        reset_pin.enter_reset();
        Timer::after(RESET_HOLD).await;

        info!("miner: enabling PSU");
        let _ = psu.enable().await;
        // Fans follow the rail, not the init-replay completion: the
        // chip is dissipating power as soon as the PSU is on, and
        // init replay can take long enough for thermals to climb.
        MINER_IS_RUNNING.signal(true);
        Timer::after(RESET_HOLD).await;

        info!("miner: exiting reset");
        reset_pin.exit_reset();
        Timer::after(RESET_HOLD).await;

        let _ = SERIAL.reset().await;

        info!("miner: replaying init ({} steps)", INIT.len());
        // Init replay is the longest single step in bring-up. A
        // STOP_MINING raised mid-replay must abort it and cut the
        // rail rather than letting the chip sit at voltage until the
        // replay finishes.
        match select(replay(&SERIAL, INIT), STOP_MINING.wait()).await {
            Either::First(Ok(())) => {}
            Either::First(Err(kind)) => {
                error!(
                    "miner: init replay failed ({:?}); cutting rail and retrying",
                    Debug2Format(&kind)
                );
                reset_pin.enter_reset();
                let _ = psu.disable().await;
                MINER_IS_RUNNING.signal(false);
                // Cancellable sleep — STOP_MINING during recovery exits
                // immediately rather than burning one more bring-up cycle.
                match select(Timer::after(RETRY_DELAY), STOP_MINING.wait()).await {
                    Either::First(()) => continue,
                    Either::Second(()) => {
                        info!("miner: stop signalled during recovery");
                        return;
                    }
                }
            }
            Either::Second(()) => {
                info!("miner: stop signalled during init replay; cutting rail");
                reset_pin.enter_reset();
                let _ = psu.disable().await;
                MINER_IS_RUNNING.signal(false);
                return;
            }
        }

        // -- mine --
        info!("miner: mining; awaiting STOP_MINING");
        STOP_MINING.wait().await;

        // -- teardown --
        info!("miner: replaying deinit ({} steps)", DEINIT.len());
        let _ = replay(&SERIAL, DEINIT).await;

        let _ = SERIAL.stop_sending_work().await;
        let _ = SERIAL.reset().await;
        reset_pin.enter_reset();
        let _ = psu.disable().await;
        // Mirror of the bring-up: keep fans on until the rail is
        // actually down, since deinit replay also runs hot.
        MINER_IS_RUNNING.signal(false);
        info!("miner: stopped");
        return;
    }
}
