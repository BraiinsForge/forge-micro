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

use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use ubos_logging::{Debug2Format, error, info, warn};
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
        match settle_or_abort(RESET_HOLD, psu, reset_pin).await {
            Settled::Continue => {}
            Settled::Stop => return,
            Settled::Halt(value) => {
                halted = value;
                continue;
            }
        }

        info!("miner: enabling PSU");
        let _ = psu.enable().await;
        // Fans follow the rail, not the init-replay completion: the
        // chip is dissipating power as soon as the PSU is on, and
        // init replay can take long enough for thermals to climb.
        MINER_IS_RUNNING.signal(true);
        match settle_or_abort(RESET_HOLD, psu, reset_pin).await {
            Settled::Continue => {}
            Settled::Stop => return,
            Settled::Halt(value) => {
                halted = value;
                continue;
            }
        }

        info!("miner: exiting reset");
        reset_pin.exit_reset();
        match settle_or_abort(RESET_HOLD, psu, reset_pin).await {
            Settled::Continue => {}
            Settled::Stop => return,
            Settled::Halt(value) => {
                halted = value;
                continue;
            }
        }

        match init_replay_phase(psu, reset_pin).await {
            InitPhase::Done => {}
            InitPhase::Stop => return,
            InitPhase::Halt(value) => {
                halted = value;
                continue;
            }
            InitPhase::Retry => continue,
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

/// Outcome of the init-replay phase. Mirrors the three actions the
/// run loop must take afterwards plus a `Retry` for a transient
/// replay failure that has already cut the rail.
enum InitPhase {
    Done,
    Stop,
    Halt(bool),
    Retry,
}

/// Drive `INIT` over the serial link with STOP and HALT cancellation.
/// On replay failure the rail is cut and a `RETRY_DELAY` settle is
/// applied (also cancellable) before reporting `Retry`. STOP and HALT
/// cut the rail before returning so the caller only dispatches on the
/// variant.
async fn init_replay_phase<P, R>(psu: &P, reset_pin: &mut R) -> InitPhase
where
    P: Psu,
    R: ResetPin,
{
    let _ = SERIAL.reset().await;

    info!("miner: replaying init ({} steps)", INIT.len());
    match select3(
        replay(&SERIAL, INIT),
        STOP_MINING.wait(),
        HALT_MINING.wait(),
    )
    .await
    {
        Either3::First(Ok(())) => InitPhase::Done,
        Either3::First(Err(kind)) => {
            error!(
                "miner: init replay failed ({:?}); cutting rail and retrying",
                Debug2Format(&kind)
            );
            cut_rail(psu, reset_pin).await;
            match select3(
                Timer::after(RETRY_DELAY),
                STOP_MINING.wait(),
                HALT_MINING.wait(),
            )
            .await
            {
                Either3::First(()) => InitPhase::Retry,
                Either3::Second(()) => {
                    info!("miner: stop signalled during recovery");
                    InitPhase::Stop
                }
                Either3::Third(value) => InitPhase::Halt(value),
            }
        }
        Either3::Second(()) => {
            info!("miner: stop signalled during init replay; cutting rail");
            cut_rail(psu, reset_pin).await;
            InitPhase::Stop
        }
        Either3::Third(value) => {
            warn!("miner: halt asserted during init replay; cutting rail");
            cut_rail(psu, reset_pin).await;
            InitPhase::Halt(value)
        }
    }
}

/// Outcome of a cancellable settling sleep in the bring-up sequence.
enum Settled {
    /// The timer elapsed without interruption; the caller proceeds
    /// to the next bring-up step.
    Continue,
    /// `STOP_MINING` fired during the sleep; the rail has been cut
    /// and the run loop must return.
    Stop,
    /// `HALT_MINING` fired during the sleep; the rail has been cut
    /// and the carried value is the new `halted` state for the run
    /// loop to resume from.
    Halt(bool),
}

/// Cancellable settling sleep used between bring-up steps. STOP or
/// HALT raised during a 5-second hold must short-circuit so the rail
/// does not keep climbing toward `enable_PSU` after a thermal trip.
/// `Stop` and `Halt` outcomes drop the rail before returning so the
/// caller only needs to dispatch on the variant.
async fn settle_or_abort<P, R>(duration: Duration, psu: &P, reset_pin: &mut R) -> Settled
where
    P: Psu,
    R: ResetPin,
{
    match select3(
        Timer::after(duration),
        STOP_MINING.wait(),
        HALT_MINING.wait(),
    )
    .await
    {
        Either3::First(()) => Settled::Continue,
        Either3::Second(()) => {
            cut_rail(psu, reset_pin).await;
            Settled::Stop
        }
        Either3::Third(value) => {
            cut_rail(psu, reset_pin).await;
            Settled::Halt(value)
        }
    }
}
