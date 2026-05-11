//! BMM100 mining main loop.
//!
//! Each iteration brings the chip up (PSU, reset sequencing, init
//! replay), waits on [`STOP_MINING`] or [`HALT_MINING`], and tears it
//! down (deinit replay, work stop, PSU off). On a fatal init failure
//! (chip goes silent at a new line rate, etc.) the iteration cuts the
//! rail, sleeps, and starts over so a transient bring-up failure does
//! not leave the chip powered without supervision.
//!
//! `HALT_MINING` is the thermal-emergency path: when asserted the loop
//! drops the rail immediately (no deinit replay, the board is already
//! over `dangerous_temp_c`) and waits for the monitor to clear the
//! halt before attempting bring-up again. `STOP_MINING` remains the
//! orderly-shutdown path that runs the deinit replay.

use embassy_futures::select::{select, select3, Either, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use ubos_logging::{error, info, warn, Debug2Format};
use ubos_monitor::HALT_MINING;
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
    let mut halted = false;

    loop {
        if halted {
            info!("miner: halt active; awaiting clear");
            match wait_halt_clear_or_stop().await {
                Resumed::Cleared => {
                    info!("miner: halt cleared; restarting bring-up");
                    halted = false;
                }
                Resumed::Stopped => return,
            }
        }

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
        // Init replay is the longest single step in bring-up. STOP or
        // HALT raised mid-replay must abort it and cut the rail rather
        // than letting the chip sit at voltage until the replay
        // finishes.
        match select3(replay(&SERIAL, INIT), STOP_MINING.wait(), HALT_MINING.wait()).await {
            Either3::First(Ok(())) => {}
            Either3::First(Err(kind)) => {
                error!(
                    "miner: init replay failed ({:?}); cutting rail and retrying",
                    Debug2Format(&kind)
                );
                cut_rail(psu, reset_pin).await;
                // Cancellable sleep — STOP or HALT during recovery
                // exits or pivots rather than burning a bring-up cycle.
                match select3(Timer::after(RETRY_DELAY), STOP_MINING.wait(), HALT_MINING.wait()).await {
                    Either3::First(()) => continue,
                    Either3::Second(()) => {
                        info!("miner: stop signalled during recovery");
                        return;
                    }
                    Either3::Third(value) => {
                        halted = value;
                        continue;
                    }
                }
            }
            Either3::Second(()) => {
                info!("miner: stop signalled during init replay; cutting rail");
                cut_rail(psu, reset_pin).await;
                return;
            }
            Either3::Third(value) => {
                warn!("miner: halt asserted during init replay; cutting rail");
                cut_rail(psu, reset_pin).await;
                halted = value;
                continue;
            }
        }

        // -- mine --
        info!("miner: mining; awaiting STOP_MINING or HALT_MINING");
        match select(STOP_MINING.wait(), HALT_MINING.wait()).await {
            Either::First(()) => {
                // -- teardown (orderly stop) --
                info!("miner: replaying deinit ({} steps)", DEINIT.len());
                let _ = replay(&SERIAL, DEINIT).await;
                let _ = SERIAL.stop_sending_work().await;
                let _ = SERIAL.reset().await;
                cut_rail(psu, reset_pin).await;
                info!("miner: stopped");
                return;
            }
            Either::Second(value) => {
                warn!("miner: halt asserted while mining; cutting rail");
                cut_rail(psu, reset_pin).await;
                halted = value;
            }
        }
    }
}

async fn cut_rail<P, R>(psu: &P, reset_pin: &mut R)
where
    P: Psu,
    R: ResetPin,
{
    reset_pin.enter_reset();
    let _ = psu.disable().await;
    MINER_IS_RUNNING.signal(false);
}

enum Resumed {
    Cleared,
    Stopped,
}

/// Block while halted. Returns `Cleared` once the monitor publishes a
/// `false` halt state; returns `Stopped` if `STOP_MINING` fires first.
async fn wait_halt_clear_or_stop() -> Resumed {
    loop {
        match select(HALT_MINING.wait(), STOP_MINING.wait()).await {
            Either::First(false) => return Resumed::Cleared,
            Either::First(true) => {}
            Either::Second(()) => return Resumed::Stopped,
        }
    }
}
