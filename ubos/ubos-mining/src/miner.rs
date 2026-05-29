//! BMM100 mining main loop.
//!
//! Each iteration brings the chip up (PSU, reset sequencing, init
//! replay), waits on [`STOP_MINING`] or a [`Message`] delivered on
//! the command channel, and tears it down (deinit replay, work stop,
//! PSU off). On a fatal init failure (chip goes silent at a new line
//! rate, etc.) the iteration cuts the rail, sleeps, and starts over
//! so a transient bring-up failure does not leave the chip powered
//! without supervision.
//!
//! `Message::Halt` is the thermal-emergency path: when delivered
//! the loop drops the rail immediately (no deinit replay, the board
//! is already over `dangerous_temp_c`) and waits for
//! `Message::Resume` before attempting bring-up again. `STOP_MINING`
//! remains the orderly-shutdown path that runs the deinit replay.

use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use ubos_core::consts::Mining;
use ubos_core::units::Voltage;
use ubos_logging::{Debug2Format, error, info, warn};
use ubos_psu::Psu;

use crate::bmm100_trace::{DEINIT, INIT};
use crate::control::{COMMAND_CHANNEL, Message, MiningState, STATE_WATCH};
use crate::replay::replay;
use crate::reset::ResetPin;
use crate::serial::SERIAL;

/// Default ASIC core voltage that the firmware programs at boot.
/// Aliased from `ubos_core::consts::Mining::DEFAULT_VOLTAGE` so the
/// boot-default voltage has one source of truth shared with
/// `ubos-config::Config::default()` and the `ubos-api` schema.
pub const VOLTAGE: Voltage = Mining::DEFAULT_VOLTAGE;

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

/// Idle counterpart to [`run`] for deployments without a real chip.
///
/// Publishes the initial `MiningState` so subscribers see a defined value,
/// then drains the command channel forever. Used by host-runner when no PSU
/// is configured: thermal still publishes temperatures and the API still
/// renders state, while `halt`/`resume` edges from thermal are absorbed
/// instead of back-pressuring the sender once the channel fills.
pub async fn run_idle() -> ! {
    publish_running(false);
    info!("mining: idle loop active (no chip); commands will be dropped");
    loop {
        let _ = COMMAND_CHANNEL.receive().await;
    }
}

/// Drive the BMM100 mining work loop.
///
/// Brings the chip up via the PSU and reset sequencing, mines until a
/// halt or stop is observed, tears down on `Message::Resume`, and
/// retries on transient init failures. Returns when `STOP_MINING`
/// fires; on bare metal that never happens, so the loop runs forever.
pub async fn run<P, R>(psu: &P, reset_pin: &mut R)
where
    P: Psu,
    R: ResetPin,
{
    // Publish the initial state so a fan task that started before us
    // sees `false` instead of waiting forever for the first edge.
    publish_running(false);
    let mut halted = false;

    loop {
        if halted {
            info!("mining: halt active; awaiting clear");
            match wait_halt_clear_or_stop().await {
                Resumed::Cleared => {
                    info!("mining: halt cleared; restarting bring-up");
                    halted = false;
                }
                Resumed::Stopped => return,
            }
        }

        // -- bring-up --
        info!("mining: setting voltage to {} mV", VOLTAGE.mv());
        let _ = psu.set_voltage(VOLTAGE).await;

        info!("mining: entering reset");
        reset_pin.enter_reset();
        match settle_or_abort(RESET_HOLD, psu, reset_pin).await {
            Settled::Continue => {}
            Settled::Stop => return,
            Settled::Halt(value) => {
                halted = value;
                continue;
            }
        }

        info!("mining: enabling PSU");
        let _ = psu.enable().await;
        // Fans follow the rail, not the init-replay completion: the
        // chip is dissipating power as soon as the PSU is on, and
        // init replay can take long enough for thermals to climb.
        publish_running(true);
        match settle_or_abort(RESET_HOLD, psu, reset_pin).await {
            Settled::Continue => {}
            Settled::Stop => return,
            Settled::Halt(value) => {
                halted = value;
                continue;
            }
        }

        info!("mining: exiting reset");
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
        info!("mining: mining; awaiting STOP_MINING or Message");
        match select(STOP_MINING.wait(), next_halt_change()).await {
            Either::First(()) => {
                // -- teardown (orderly stop) --
                info!("mining: replaying deinit ({} steps)", DEINIT.len());
                let _ = replay(&SERIAL, DEINIT).await;
                let _ = SERIAL.stop_sending_work().await;
                let _ = SERIAL.reset().await;
                cut_rail(psu, reset_pin).await;
                info!("mining: stopped");
                return;
            }
            Either::Second(true) => {
                warn!("mining: halt asserted while mining; cutting rail");
                cut_rail(psu, reset_pin).await;
                halted = true;
            }
            Either::Second(false) => {}
        }
    }
}

/// Publish a `MiningState` reflecting the rail flag. `chip_count` and
/// `target` are placeholders until 3f/Step 6 wires the tuner and
/// chain enumeration.
fn publish_running(running: bool) {
    STATE_WATCH.sender().send(MiningState {
        running,
        chip_count: 0,
        target: None,
    });
}

/// Block until a `Message::Halt`/`Resume` arrives. Returns the
/// resulting halt state. Other command variants are not yet implemented
/// and are dropped with a warn so producers can be migrated in 3e
/// without a behavior break here.
async fn next_halt_change() -> bool {
    loop {
        match COMMAND_CHANNEL.receive().await {
            Message::Halt => return true,
            Message::Resume => return false,
            Message::SetTarget { freq, voltage } => {
                warn!(
                    "mining: dropping SetTarget({} MHz, {} mV); tuner lands in Step 6",
                    freq.mhz(),
                    voltage.mv()
                );
            }
            #[cfg(feature = "legacy-replay")]
            Message::ExecuteStep(_) => {
                warn!("mining: dropping ExecuteStep; replay path stays inside run() until Step 6");
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
    publish_running(false);
}

enum Resumed {
    Cleared,
    Stopped,
}

/// Block while halted. Returns `Cleared` once a `Message::Resume`
/// edge is observed; returns `Stopped` if `STOP_MINING` fires first.
async fn wait_halt_clear_or_stop() -> Resumed {
    loop {
        match select(next_halt_change(), STOP_MINING.wait()).await {
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

    info!("mining: replaying init ({} steps)", INIT.len());
    match select3(
        replay(&SERIAL, INIT),
        STOP_MINING.wait(),
        next_halt_change(),
    )
    .await
    {
        Either3::First(Ok(())) => InitPhase::Done,
        Either3::First(Err(kind)) => {
            error!(
                "mining: init replay failed ({:?}); cutting rail and retrying",
                Debug2Format(&kind)
            );
            cut_rail(psu, reset_pin).await;
            match select3(
                Timer::after(RETRY_DELAY),
                STOP_MINING.wait(),
                next_halt_change(),
            )
            .await
            {
                Either3::First(()) => InitPhase::Retry,
                Either3::Second(()) => {
                    info!("mining: stop signalled during recovery");
                    InitPhase::Stop
                }
                Either3::Third(value) => InitPhase::Halt(value),
            }
        }
        Either3::Second(()) => {
            info!("mining: stop signalled during init replay; cutting rail");
            cut_rail(psu, reset_pin).await;
            InitPhase::Stop
        }
        Either3::Third(value) => {
            warn!("mining: halt asserted during init replay; cutting rail");
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
    /// A `Message::Halt` edge fired during the sleep; the rail has
    /// been cut and the carried value is the new `halted` state for
    /// the run loop to resume from.
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
        next_halt_change(),
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
