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

//! Multi-chain mining lifecycle and execution of synchronous tuner plans.
//!
//! Bring-up holds the chains in reset with the rail off until a pool slot holds a job,
//! then configures every chain concurrently and parks absent or faulted connectors.
//! A run in which no pool slot holds a job for `POOL_LOSS_GRACE` cuts the rail and
//! waits for a pool again.
//! The miner applies bounded policy plans in decrease, rail, increase order.
//! Live targets take effect at the next step. Interruptions allow the current plan
//! a bounded wait, then cut the rail without a shutdown ramp.

#[cfg(test)]
#[path = "tests/miner.rs"]
mod tests;

use core::cmp::Ordering;
use core::mem::{Discriminant, discriminant};

use embassy_futures::join::join_array;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_io::ErrorKind;
use ubos_asic::HashboardDescriptor;
use ubos_config::{ConfigSubscription, Tuning};
use ubos_core::units::{Frequency, Temperature, Voltage};
use ubos_logging::{Debug2Format, error, info, warn};
use ubos_metrics::chain::ChainPresence;
use ubos_psu::{Error as PsuError, Psu};

use crate::chain::{ChainControl, ChainHandle};
use crate::control::{Handle, Message, MiningState};
use crate::reset::ResetPin;
use crate::serial::Serial;
use crate::tuner::MachineTuner;
use ubos_tuner::{ChainReadings, Plan, Readings};

/// Maximum time an interrupted hardware plan can finish before the rail cut.
const INTERRUPT_TIMEOUT: Duration = Duration::from_millis(100);

/// Plans in a row that may fail before the miner treats the tuner as
/// faulted and re-runs bring-up. A plan that writes to the hardware
/// clears the count; one that writes nothing, a wait or a value already
/// held, leaves it.
const PLAN_FAILURE_BUDGET: u8 = 3;
/// How often the miner asks the actor whether a pool slot holds a job,
/// both while waiting for a pool and while watching for its loss.
const POOL_POLL_PERIOD: Duration = Duration::from_secs(1);

/// How long every pool slot may stay empty while mining before the rail
/// is cut. Long enough for a pool to reconnect without a power cycle.
const POOL_LOSS_GRACE: Duration = Duration::from_secs(60);

/// Request shutdown during initialization, plan execution or settling.
/// Teardown cuts the rail before the outer loop returns.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub static STOP_MINING: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Run the mining module of a deployment that has no chip: publish
/// [`MiningState::Stopped`] and drain halt and resume.
///
/// The command channel is bounded, so a deployment that never reads it
/// blocks every later sender, which is the manager task.
pub async fn run_idle() -> ! {
    let handle = crate::handle();
    handle.publish(MiningState::Stopped);
    info!("mining: idle loop active (no chip)");
    loop {
        handle.next_message().await;
    }
}

/// Error type for chip-control operations.
///
/// Each variant represents a distinct failure mode the caller can
/// observe and dispatch on. `Serial` wraps the `embedded_io` error
/// kind so the miner can surface a consistent error surface without
/// coupling callers to the specific serial actor types.
#[derive(Debug)]
pub enum MinerError {
    /// The serial actor returned an error on a control operation.
    Serial(ErrorKind),
    /// The PSU rejected a voltage or enable/disable command. The
    /// payload narrows the failure to a discriminant the caller can
    /// dispatch on without taking on the PSU's `BusError` generic.
    Psu(PsuErrorKind),
    /// The chain driver rejected a frequency outside the chip's range.
    UnsupportedFrequency(Frequency),
}

impl From<ubos_asic::ChipError> for MinerError {
    fn from(error: ubos_asic::ChipError) -> Self {
        match error {
            ubos_asic::ChipError::UnsupportedFrequency(freq) => {
                MinerError::UnsupportedFrequency(freq)
            }
            // A refused host baud switch is a serial-line failure; the
            // rate itself is already logged by the transport.
            ubos_asic::ChipError::BaudSwitch(_) => MinerError::Serial(ErrorKind::Other),
        }
    }
}

impl From<embedded_io::ErrorKind> for MinerError {
    fn from(kind: embedded_io::ErrorKind) -> Self {
        MinerError::Serial(kind)
    }
}

/// Classification of [`ubos_psu::Error`] surfaces stripped of the
/// generic bus-error payload so [`MinerError`] does not have to be
/// parameterized over the PSU's `BusError` type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PsuErrorKind {
    Bus,
    InitFailed,
    OutOfRange,
    Fault,
    Terminated,
    Timeout,
}

impl<E> From<PsuError<E>> for PsuErrorKind {
    fn from(error: PsuError<E>) -> Self {
        match error {
            PsuError::Bus(_) => PsuErrorKind::Bus,
            PsuError::InitFailed(_) => PsuErrorKind::InitFailed,
            PsuError::OutOfRange => PsuErrorKind::OutOfRange,
            PsuError::Fault(_) => PsuErrorKind::Fault,
            PsuError::Terminated => PsuErrorKind::Terminated,
            PsuError::Timeout => PsuErrorKind::Timeout,
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for MinerError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            MinerError::Serial(kind) => {
                defmt::write!(f, "Serial({:?})", defmt::Debug2Format(kind));
            }
            MinerError::Psu(kind) => {
                defmt::write!(f, "Psu({:?})", defmt::Debug2Format(kind));
            }
            MinerError::UnsupportedFrequency(freq) => {
                defmt::write!(f, "UnsupportedFrequency({} kHz)", freq.khz());
            }
        }
    }
}

/// The ASIC mining state machine driving the injected chain driver.
///
/// Owns the PSU reference and every chain of the machine. Firmware
/// constructs this with [`Miner::new`] and calls [`Miner::run`] to
/// enter the bring-up loop. The `'a` lifetime covers the PSU reference
/// which today is always `'static` at the firmware call site. `T` is the
/// machine's tuner set.
pub struct Miner<'a, P, R, T, const CHAINS: usize>
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    psu: &'a P,
    board: &'static HashboardDescriptor,
    chains: [ChainHandle<R>; CHAINS],
    halted: bool,
    /// Readings only this board's platform has. Read once per step and
    /// passed to the tuner.
    read_platform: fn() -> T::PlatformReadings,
    /// The mining handle the API reads; the miner publishes its lifecycle
    /// status through it.
    mining: Handle,
    /// Bring-up cycles that found no chain since the last one that did.
    /// Counts toward `BRING_UP_CYCLES`, and is cleared by every cycle
    /// that brings a chain up.
    empty_cycles: u8,
}

impl<'a, P, R, T, const CHAINS: usize> Miner<'a, P, R, T, CHAINS>
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    /// Settling time held between each reset / PSU transition during
    /// the bring-up sequence.
    const RESET_HOLD: Duration = Duration::from_secs(5);

    /// Pause between a failed bring-up and the next attempt. Long enough
    /// for the chip's rail to fully discharge and any transient cable /
    /// board glitch to clear.
    const RETRY_DELAY: Duration = Duration::from_secs(5);

    /// Bring-up cycles the machine spends while no chain comes up
    /// present, before it parks instead of cutting the rail again.
    ///
    /// Each cycle already gives every chain `ENUMERATE_ATTEMPTS` tries
    /// of its own, so this is the second and outer budget: it buys the
    /// rail cut that a chain-level retry cannot perform, and bounds it,
    /// because a machine with nothing behind its connectors otherwise
    /// cycles the rail for as long as it is powered, once per
    /// `RESET_HOLD` sequence, chain retry budget and `RETRY_DELAY`.
    const BRING_UP_CYCLES: u8 = 3;

    /// Construct a `Miner` from a PSU reference, the board descriptor
    /// whose chip facts drive the tuner, one serial actor with its reset
    /// pin per chain of the machine, in chain order, and the function that
    /// reads what the platform reports beyond the common readings.
    ///
    /// A `Serial` declares its own index and its own view of the
    /// machine's chain count, and both are checked against its position
    /// in `chains`. The reset pin travels in the same pair as its actor,
    /// so a board names the two together and there is no second array
    /// whose order could drift from the first.
    ///
    /// The PSU is borrowed for lifetime `'a`; on all current firmware
    /// targets `'a` is `'static` because the PSU lives in a static.
    ///
    /// # Panics
    ///
    /// Panics if a `Serial` declares an index other than its position
    /// in `chains`, or a chain count other than `CHAINS`: an actor that
    /// counts the machine's chains differently partitions the version
    /// mask differently, and two chains would hash the same headers.
    /// Both fire in a release build, because a machine that is wired
    /// wrong must not mine. `CHAINS` above `ubos_metrics::chain::MAX_CHAINS`,
    /// which the per-chain accounting table has no slot for, fails to
    /// compile.
    pub fn new(
        psu: &'a P,
        board: &'static HashboardDescriptor,
        chains: [(&'static Serial, R); CHAINS],
        read_platform: fn() -> T::PlatformReadings,
    ) -> Self {
        const {
            assert!(
                CHAINS <= ubos_metrics::chain::MAX_CHAINS,
                "the machine has more chains than MAX_CHAINS"
            );
        }
        // Checked before the accounting slots are taken, not after: the
        // lookup below indexes the table by the actor's own declared
        // index, so an actor declaring one past the table would panic
        // inside the slice and never reach the assertion that names the
        // wiring mistake.
        for (position, (serial, _)) in chains.iter().enumerate() {
            assert_eq!(
                serial.chain(),
                position,
                "BUG: chain {position} wired to a Serial that declares another index"
            );
            assert_eq!(
                serial.chains(),
                CHAINS,
                "BUG: chain {position} wired to a Serial that counts another number of chains"
            );
        }
        let chains = chains.map(|(serial, reset)| ChainHandle {
            serial,
            stats: ubos_metrics::chain::handle().chain(serial.chain()),
            reset,
        });
        ubos_metrics::chain::set_chain_count(CHAINS);
        Self {
            psu,
            board,
            chains,
            halted: false,
            read_platform,
            mining: crate::handle(),
            empty_cycles: 0,
        }
    }

    /// Drive the mining work loop.
    ///
    /// Reads the initial config from `ubos_config::handle()` and holds a
    /// subscription across the outer bring-up loop. On each iteration the
    /// outer loop reads the current config, selects the tuner
    /// `MinerConfig::tuning` names, and calls `bring_up_cycle`. Returns
    /// when `STOP_MINING` fires; on bare metal that never happens, so the
    /// loop runs forever.
    ///
    /// A configuration that selects a tuner this machine does not have
    /// leaves the rail off until the next configuration.
    ///
    /// # Tuner-change invariant
    ///
    /// Switching tuners cuts the rail and resets every chain.
    /// The next cycle constructs a fresh policy from the post-reset baseline.
    /// A new target for the running tuner does not: it reaches the tuner
    /// at its next step.
    pub async fn run(&mut self) {
        let config_handle = ubos_config::handle();
        let mut config_subscription = config_handle.subscribe();

        // Block for the config task's first publish before reading a
        // target. This task is spawned before the config task and reaches
        // the read below with no intervening await, so `current()` would
        // otherwise fall back to `Config::default()` — the BMM101/BM1362
        // board's operating point, wrong for this chip. Returns
        // immediately once a config has been published. The loop reads
        // the latest target again before every policy step.
        config_subscription.changed().await;

        // Park the rail off before the first reset, so every board
        // begins bring-up from a known-off supply regardless of what
        // the previous boot cycle left it in. A failure is logged and
        // bring-up goes on: the enable that follows the reset sets the
        // rail either way.
        if let Err(error) = self.psu.disable().await {
            warn!(
                "psu disable before the first reset failed: {:?}",
                Debug2Format(&error)
            );
        }

        loop {
            let tuning = config_handle.current().miner.tuning;
            let chosen = T::select(&tuning, self.board);
            let outcome = if let Some(mut tuner) = chosen {
                self.bring_up_cycle(&mut tuner, &mut config_subscription, discriminant(&tuning))
                    .await
            } else {
                error!("mining: this machine has no tuner for the configured tuning");
                self.await_another_tuner(&mut config_subscription, discriminant(&tuning))
                    .await
            };
            if outcome.is_stop() {
                return;
            }
        }
    }

    /// Hold until the configuration names a tuner this machine has, or
    /// `STOP_MINING` fires.
    ///
    /// The rail is cut first rather than assumed off. The hold can be
    /// entered on the first pass through `run`, where the only thing
    /// that has touched the supply is a disable whose failure is logged
    /// and ignored, so a board that boots under an unusable tuning would
    /// otherwise hold with a rail nobody confirmed down.
    ///
    /// Halt and resume messages are read as they arrive and the last
    /// edge is kept. Reading them is not optional: the hold ends only on
    /// a configuration or a stop, so it can last indefinitely, and
    /// `COMMAND_CHANNEL` is bounded, so a state that never drains it
    /// blocks every later sender — which is the manager task the halt
    /// came from. Keeping the edge is not optional either: an operator
    /// who halts the machine during the hold must still find it halted
    /// when a usable tuning arrives.
    async fn await_another_tuner(
        &mut self,
        subscription: &mut ConfigSubscription,
        selected: Discriminant<Tuning>,
    ) -> CycleOutcome {
        cut_rail(self.psu, &mut self.chains).await;
        let mut halted = self.halted;
        let stopped = {
            let track_halt = async {
                loop {
                    halted = next_halt_change().await;
                }
            };
            matches!(
                select3(
                    STOP_MINING.wait(),
                    tuner_changed(selected, subscription),
                    track_halt,
                )
                .await,
                Either3::First(())
            )
        };
        self.halted = halted;
        if stopped {
            CycleOutcome::Stop
        } else {
            CycleOutcome::Continue
        }
    }

    /// Reset, enumerate and configure every chain, all at once,
    /// cancellable by STOP and HALT the same way `start_work_phase` is.
    /// This is the last bring-up step before the tuner sees a chain.
    ///
    /// A chain whose count never settles on `self.board.num_chips`
    /// within the retry budget is marked absent when its last answer was
    /// no chips at all, and faulted when it was a count, a sweep no chip
    /// answered, or a refused safe-boot configuration. Either way the
    /// chain is parked in reset and left out of the ramp, and the other
    /// chains carry on: only a bring-up where no chain comes up present
    /// cuts the rail.
    async fn bring_up_chains_phase(&mut self) -> InitPhase {
        let expected = self.board.num_chips;
        match select3(
            STOP_MINING.wait(),
            wait_for_halt(),
            bring_up_all(&mut self.chains, expected),
        )
        .await
        {
            Either3::Third(BringUpOutcome::Done) => {
                self.empty_cycles = 0;
                InitPhase::Done
            }
            Either3::Third(BringUpOutcome::NonePresent) => self.no_chain_present().await,
            Either3::First(()) => {
                info!("mining: stop signalled during chain bring-up; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Stop
            }
            Either3::Second(()) => {
                warn!("mining: halt asserted during chain bring-up; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Halted
            }
        }
    }

    /// Count a bring-up cycle that brought no chain up, and decide
    /// between another cycle and a park.
    ///
    /// The rail cut is what a chain-level retry cannot do, and both
    /// failures that this budget exists for cleared on the cycle after
    /// one. `Self::BRING_UP_CYCLES` bounds it: a machine whose
    /// connectors are empty parks instead of cutting its rail for as
    /// long as it is powered. A parked machine waits for the same
    /// resume a halt waits for. The manager sends one for a Start
    /// that finds nothing paused, so an operator sends Start, or
    /// reboots the board.
    async fn no_chain_present(&mut self) -> InitPhase {
        if !self.empty_cycle_spends_the_budget() {
            error!(
                "mining: no chain present on cycle {} of {}; cutting rail and retrying",
                self.empty_cycles,
                Self::BRING_UP_CYCLES
            );
            return self.cut_rail_and_settle().await;
        }
        error!(
            "mining: no chain present in {} cycles; parking until resume",
            Self::BRING_UP_CYCLES
        );
        crate::events::chains_parked(Self::BRING_UP_CYCLES);
        cut_rail(self.psu, &mut self.chains).await;
        InitPhase::Parked
    }

    /// Count one bring-up cycle that brought no chain up, and report
    /// whether that spends `Self::BRING_UP_CYCLES`.
    ///
    /// Separate from [`Self::no_chain_present`], and free of I/O, so
    /// the budget is testable without the rail cut and the settle that
    /// surround it.
    fn empty_cycle_spends_the_budget(&mut self) -> bool {
        self.empty_cycles = self.empty_cycles.saturating_add(1);
        self.empty_cycles >= Self::BRING_UP_CYCLES
    }

    /// Cut the rail after a transient bring-up failure and hold
    /// `Self::RETRY_DELAY` before the next attempt, cancellable by STOP
    /// and HALT. Returns `Retry` when the hold elapses.
    async fn cut_rail_and_settle(&mut self) -> InitPhase {
        cut_rail(self.psu, &mut self.chains).await;
        match select3(
            STOP_MINING.wait(),
            wait_for_halt(),
            Timer::after(Self::RETRY_DELAY),
        )
        .await
        {
            Either3::Third(()) => InitPhase::Retry,
            Either3::First(()) => {
                info!("mining: stop signalled during recovery");
                InitPhase::Stop
            }
            Either3::Second(()) => InitPhase::Halted,
        }
    }

    /// Anchor the shared rail and start every present chain before tuning.
    async fn start_work_phase(&mut self) -> InitPhase {
        info!("mining: running work initialization");
        let init_result = select3(STOP_MINING.wait(), wait_for_halt(), self.start_chains()).await;
        match init_result {
            Either3::Third(Ok(())) => {
                crate::events::work_initialized();
                InitPhase::Done
            }
            Either3::Third(Err(error)) => {
                error!(
                    "mining: work initialization error ({:?}); cutting rail and retrying",
                    Debug2Format(&error)
                );
                crate::events::work_initialization_failed();
                self.cut_rail_and_settle().await
            }
            Either3::First(()) => {
                info!("mining: stop signalled during work initialization; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Stop
            }
            Either3::Second(()) => {
                warn!("mining: halt asserted during work initialization; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Halted
            }
        }
    }

    /// One bring-up + mine cycle for a single tuner instance.
    ///
    /// Runs the PSU-reset-settle-enumerate-configure-init sequence over every
    /// chain, then delegates to `mine_phase` for the steady-state
    /// select loop. Returns `CycleOutcome::Stop` when `STOP_MINING`
    /// fires; returns `CycleOutcome::Continue` on halt, halt-clear, a
    /// tuner change, or a transient init failure that has already cut the rail.
    ///
    /// `selected` is the tuning variant `tuner` was built from, which the
    /// mining loop compares the live configuration against.
    async fn bring_up_cycle(
        &mut self,
        tuner: &mut T,
        config_subscription: &mut ConfigSubscription,
        selected: Discriminant<Tuning>,
    ) -> CycleOutcome {
        if self.halted {
            info!("mining: rail down; awaiting resume");
            match wait_halt_clear_or_stop().await {
                Resumed::Cleared => {
                    info!("mining: resumed; restarting bring-up");
                    self.halted = false;
                    // An operator who resumes asks for the whole budget
                    // again. Without this the first cycle after a park
                    // parks the machine a second time.
                    self.empty_cycles = 0;
                }
                Resumed::Stopped => return CycleOutcome::Stop,
            }
        }

        self.mining.publish(MiningState::WaitingForPool);
        info!("mining: entering reset");
        crate::events::miner_reset();
        for chain in &mut self.chains {
            chain.reset.enter_reset().await;
        }
        info!("mining: waiting for a pool to hold a job");
        let serial = self.pool_serial();
        let pool = wait_for_pool_or_abort(self.psu, serial, &mut self.chains).await;
        if let Some(outcome) = self.leave_cycle(pool) {
            return outcome;
        }
        crate::events::pool_available();

        self.mining.publish(MiningState::Starting);
        let settled = settle_or_abort(Self::RESET_HOLD, self.psu, &mut self.chains).await;
        if let Some(outcome) = self.leave_cycle(settled) {
            return outcome;
        }

        info!("mining: enabling PSU");
        crate::events::psu_enabled();
        if let Err(error) = self.psu.enable().await {
            warn!("psu enable failed: {:?}", Debug2Format(&error));
        }
        let settled = settle_or_abort(Self::RESET_HOLD, self.psu, &mut self.chains).await;
        if let Some(outcome) = self.leave_cycle(settled) {
            return outcome;
        }

        info!("mining: exiting reset");
        for chain in &mut self.chains {
            chain.reset.exit_reset().await;
        }
        let settled = settle_or_abort(Self::RESET_HOLD, self.psu, &mut self.chains).await;
        if let Some(outcome) = self.leave_cycle(settled) {
            return outcome;
        }

        // Cleanup owns an accepted STOP or HALT until leave_cycle records it.
        // A configuration publication must not cancel that cleanup.
        let brought_up = self.bring_up_chains_phase().await;
        if let Some(outcome) = self.leave_cycle(brought_up) {
            return outcome;
        }
        let initialised = self.start_work_phase().await;
        if let Some(outcome) = self.leave_cycle(initialised) {
            return outcome;
        }

        // Every present chain is enumerated, configured and clocked;
        // from here it only hashes. A board that addresses it
        // differently for the two moves its mode line now.
        for chain in &mut self.chains {
            if chain.is_present() {
                chain.reset.enter_work_mode().await;
            }
        }

        self.mining.publish(MiningState::Running);
        self.mine_phase(tuner, config_subscription, selected).await
    }

    /// Read the live target at each step and retain the first accepted interruption.
    async fn mine_phase(
        &mut self,
        tuner: &mut T,
        subscription: &mut ConfigSubscription,
        selected: Discriminant<Tuning>,
    ) -> CycleOutcome {
        let mut plan_delay = Duration::from_millis(0);
        let mut consecutive_plan_failures: u8 = 0;
        let mut pool = PoolWatch::default();
        let serial = self.pool_serial();
        let interrupt = loop {
            match select3(
                interrupted(selected, subscription),
                Timer::after(plan_delay),
                pool.lost(serial),
            )
            .await
            {
                Either3::First(exit) => break exit,
                Either3::Second(()) => {}
                Either3::Third(()) => {
                    warn!("mining: no pool has held a job for the grace period; cutting rail");
                    crate::events::pool_lost();
                    cut_rail(self.psu, &mut self.chains).await;
                    return CycleOutcome::Continue;
                }
            }
            let snapshot = subscription.get().await;
            if discriminant(&snapshot.miner.tuning) != selected {
                break Interrupt::Restart;
            }
            let plan = tuner.step(
                &snapshot.miner.tuning,
                &self.readings(),
                &(self.read_platform)(),
            );
            plan_delay = plan.delay;
            let mut interruption = None;
            match select(
                async {
                    interruption = Some(interrupted(selected, subscription).await);
                    Timer::after(INTERRUPT_TIMEOUT).await;
                },
                self.apply_plan(&plan),
            )
            .await
            {
                Either::First(()) => warn!("mining: tuner cancellation timed out"),
                Either::Second(Err(error)) if interruption.is_none() => {
                    consecutive_plan_failures += 1;
                    warn!(
                        "mining: plan failed ({} of {}): {:?}",
                        consecutive_plan_failures,
                        PLAN_FAILURE_BUDGET,
                        Debug2Format(&error)
                    );
                    if consecutive_plan_failures >= PLAN_FAILURE_BUDGET {
                        crate::events::tuner_fault();
                        let phase = self.cut_rail_and_settle().await;
                        // `cut_rail_and_settle` never returns `Done`, so the
                        // fallback stands only to keep the expression total.
                        return self.leave_cycle(phase).unwrap_or(CycleOutcome::Continue);
                    }
                }
                Either::Second(Err(error)) => warn!(
                    "mining: interrupted plan failed: {:?}",
                    Debug2Format(&error)
                ),
                Either::Second(Ok(written)) => {
                    if written {
                        consecutive_plan_failures = 0;
                    }
                }
            }
            if let Some(exit) = interruption {
                break exit;
            }
        };
        cut_rail(self.psu, &mut self.chains).await;
        match interrupt {
            Interrupt::Stop => CycleOutcome::Stop,
            Interrupt::Halt => {
                self.halted = true;
                CycleOutcome::Continue
            }
            Interrupt::Restart => CycleOutcome::Continue,
        }
    }

    /// The actor that answers pool questions for the machine. Every
    /// chain shares one session, so chain 0 speaks for all of them, and
    /// the question touches no chip, so its own presence does not matter.
    fn pool_serial(&self) -> &'static Serial {
        self.chains[0].serial
    }

    /// Decide what `bring_up_cycle` does after one bring-up step:
    /// `None` to go on with the next step, `Some` to leave the cycle
    /// with that outcome. A halt edge records the new halt state on
    /// the way out, so the next cycle waits for the clear first.
    fn leave_cycle(&mut self, phase: InitPhase) -> Option<CycleOutcome> {
        match phase {
            InitPhase::Done => None,
            InitPhase::Stop => Some(CycleOutcome::Stop),
            InitPhase::Halted | InitPhase::Parked => {
                self.halted = true;
                Some(CycleOutcome::Continue)
            }
            InitPhase::Retry => Some(CycleOutcome::Continue),
        }
    }

    /// Set the open-core rail and start work only on configured chains.
    async fn start_chains(&self) -> Result<(), MinerError> {
        const WORK_PERIOD: Duration = Duration::from_secs(1);
        self.psu
            .set_voltage(self.board.max_rail_voltage())
            .await
            .map_err(|error| MinerError::Psu(error.into()))?;
        for chain in self.chains.iter().filter(|chain| chain.is_present()) {
            chain.start_sending_work(WORK_PERIOD).await?;
        }
        Ok(())
    }

    /// Collect each present chain's accounting and the PSU's confirmed setpoint.
    ///
    /// The setpoint is present by construction. `start_chains` commands the
    /// open-core rail and `bring_up_cycle` reaches `mine_phase` only after
    /// that command succeeded, and `Psu::set_voltage` records a successful
    /// write before it returns.
    fn readings(&self) -> Readings<CHAINS> {
        Readings {
            now: Instant::now(),
            voltage: ubos_psu::handle()
                .commanded_voltage()
                .expect("BUG: tuning starts only after a successful PSU voltage command"),
            psu: ubos_psu::handle().current(),
            chains: self.chains.each_ref().map(|chain| {
                chain.is_present().then(|| ChainReadings {
                    chips: chain.stats.chips(),
                    frequency: chain.stats.frequency(),
                    hottest: chain.stats.hottest_mc().map(Temperature::from_millicelsius),
                    hashrate: chain.stats.hashrate(),
                })
            }),
        }
    }

    /// Decreasing clocks precede the shared voltage change; increasing clocks follow it.
    ///
    /// Reports whether any command reached the hardware. A frequency for a
    /// connector with no chain, or a value the chain or the PSU already
    /// holds, is not a write.
    async fn apply_plan(&self, plan: &Plan<CHAINS>) -> Result<bool, MinerError> {
        let mut decreases = [None; CHAINS];
        let mut increases = [None; CHAINS];
        for (index, chain) in self.chains.iter().enumerate() {
            if !chain.is_present() {
                continue;
            }
            let Some(next) = plan.frequencies[index] else {
                continue;
            };
            let range = self.board.chip.frequency_range;
            let next = next.clamp(range.min(), range.max());
            match next.cmp(&chain.stats.frequency()) {
                Ordering::Less => decreases[index] = Some(next),
                Ordering::Greater => increases[index] = Some(next),
                Ordering::Equal => {}
            }
        }

        let slowed = self.set_frequencies(decreases).await?;
        // Only the ceiling is clamped here. It is board policy that can sit
        // above the PSU's own range, so nothing below would refuse it. The
        // floor is the PSU's, and a rail under its range returns
        // `Error::OutOfRange` rather than reaching the hardware.
        let rail = self
            .set_voltage_if_different(
                plan.rail
                    .map(|rail| rail.min(self.board.max_rail_voltage())),
            )
            .await?;
        let quickened = self.set_frequencies(increases).await?;
        Ok(slowed || rail || quickened)
    }

    /// Reports whether any chain was written.
    async fn set_frequencies(
        &self,
        frequencies: [Option<Frequency>; CHAINS],
    ) -> Result<bool, MinerError> {
        let mut written = false;
        for (chain, next) in self.chains.iter().zip(frequencies) {
            if let Some(next) = next {
                chain.set_frequency(next).await?;
                written = true;
            }
        }
        Ok(written)
    }

    /// Reports whether the rail was written.
    async fn set_voltage_if_different(&self, voltage: Option<Voltage>) -> Result<bool, MinerError> {
        let Some(voltage) =
            voltage.filter(|next| Some(*next) != ubos_psu::handle().commanded_voltage())
        else {
            return Ok(false);
        };
        self.psu
            .set_voltage(voltage)
            .await
            .map_err(|error| MinerError::Psu(error.into()))?;
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interrupt {
    Stop,
    Halt,
    Restart,
}

/// STOP wins simultaneous requests, then HALT, then a tuner change.
/// An accepted request is retained through cleanup, independent of later events.
async fn interrupted(
    selected: Discriminant<Tuning>,
    subscription: &mut ConfigSubscription,
) -> Interrupt {
    match select3(
        STOP_MINING.wait(),
        wait_for_halt(),
        tuner_changed(selected, subscription),
    )
    .await
    {
        Either3::First(()) => Interrupt::Stop,
        Either3::Second(()) => Interrupt::Halt,
        Either3::Third(()) => Interrupt::Restart,
    }
}

/// Wait for a configuration that selects a tuner other than the one
/// `selected` names.
///
/// The variant alone decides, so a new target for the running tuner is
/// not a change: the mining loop hands it to the tuner at the next step
/// instead of tearing the machine down.
async fn tuner_changed(selected: Discriminant<Tuning>, subscription: &mut ConfigSubscription) {
    while discriminant(&subscription.changed().await.miner.tuning) == selected {}
}

/// Wait for a halt while the miner is not halted. Ignore redundant resume messages.
/// Return on the first halt so a queued resume reaches [`wait_halt_clear_or_stop`].
async fn wait_for_halt() {
    while !next_halt_change().await {}
}

/// Reset, enumerate and configure every chain, concurrently. Free
/// function rather than a `Miner` method so it can sit as one arm of the
/// `select3` in `bring_up_chains_phase` while `self.psu` stays available
/// for the other arms' `cut_rail` calls.
///
/// What one chain does is [`ChainHandle::bring_up`]; this runs them at
/// once and reports whether any came up present.
async fn bring_up_all<R: ResetPin, const CHAINS: usize>(
    chains: &mut [ChainHandle<R>; CHAINS],
    expected: u8,
) -> BringUpOutcome {
    join_array(chains.each_mut().map(|chain| chain.bring_up(expected))).await;
    if chains.iter().any(ChainHandle::is_present) {
        BringUpOutcome::Done
    } else {
        BringUpOutcome::NonePresent
    }
}

/// Outcome of [`bring_up_all`].
enum BringUpOutcome {
    /// At least one chain came up present. An absent or faulted chain
    /// elsewhere does not change this outcome; it is only parked.
    Done,
    /// No chain came up present.
    NonePresent,
}

async fn next_halt_change() -> bool {
    match crate::handle().next_message().await {
        Message::Halt => true,
        Message::Resume => false,
    }
}

/// Put every chain in reset and drop the rail, then report them stopped.
///
/// The order matters. Reset is asserted before the rail goes, so the
/// chips are held rather than losing power mid-transaction. A failure to
/// disable the PSU is logged; the chips remain in reset.
///
/// A chain that was present is published as `Unknown` here, whichever
/// phase the cut comes from: its enumeration is void once the reset is
/// asserted, and the next bring-up decides it again. An absent or
/// faulted chain keeps its verdict, so a parked connector still reads
/// as parked while the machine is down. The store is a direct write
/// rather than an actor command, so a cut never waits behind a
/// bring-up command the actor is still running.
/// Actor scheduling and driver state are reset by `ChainHandle::bring_up`
/// before enumeration, regardless of how the previous cycle ended.
///
/// The PSU and the chain handles are two generic parameters of equal
/// standing, and neither is the receiver. No inherent `impl` in this
/// crate can name either, and the caller owns both for the life of the
/// mining loop.
async fn cut_rail<P, R>(psu: &P, chains: &mut [ChainHandle<R>])
where
    P: Psu,
    R: ResetPin,
{
    for chain in chains.iter_mut() {
        chain.reset.enter_reset().await;
        if chain.is_present() {
            chain.stats.set_presence(ChainPresence::Unknown);
        }
    }
    if let Err(error) = psu.disable().await {
        warn!("psu disable failed: {:?}", Debug2Format(&error));
    }
    crate::handle().publish(MiningState::Stopped);
}

/// Outcome returned by `bring_up_cycle` to the outer `run` loop.
enum CycleOutcome {
    /// The outer loop must return: `STOP_MINING` was signalled.
    Stop,
    /// The outer loop should restart with a fresh config snapshot:
    /// a halt/resume cycle or a tuner change triggered teardown.
    Continue,
}

impl CycleOutcome {
    fn is_stop(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

enum Resumed {
    Cleared,
    Stopped,
}

/// Block while halted. Returns `Cleared` once a `Message::Resume`
/// edge is observed; returns `Stopped` if `STOP_MINING` fires first.
async fn wait_halt_clear_or_stop() -> Resumed {
    loop {
        match select(STOP_MINING.wait(), next_halt_change()).await {
            Either::Second(false) => return Resumed::Cleared,
            Either::Second(true) => {}
            Either::First(()) => return Resumed::Stopped,
        }
    }
}

/// Outcome of one bring-up step: a settling sleep, the enumeration,
/// the configuration or work initialization. `Miner::leave_cycle`
/// turns it into the run loop's next move.
#[derive(Clone, Copy)]
enum InitPhase {
    /// The step completed; the caller proceeds to the next one.
    Done,
    /// `STOP_MINING` fired during the step; the rail has been cut and
    /// the run loop must return.
    Stop,
    /// A `Message::Halt` edge fired during the step; the rail has been
    /// cut. The next cycle waits for Resume.
    Halted,
    /// No chain came up in `Miner::BRING_UP_CYCLES` cycles; the rail has
    /// been cut. The next cycle waits for Resume, exactly as a halt
    /// does, so the machine stops cutting its rail on a loop.
    Parked,
    /// A transient failure that has already cut the rail; the cycle
    /// restarts from a fresh config snapshot.
    Retry,
}

/// Hold the chains in reset until a pool slot holds a job, cancellable
/// by STOP and HALT the same way `settle_or_abort` is. `serial` is
/// `Miner::pool_serial`, passed in because `cut_rail` needs the chains
/// mutably beside it.
async fn wait_for_pool_or_abort<P, R>(
    psu: &P,
    serial: &Serial,
    chains: &mut [ChainHandle<R>],
) -> InitPhase
where
    P: Psu,
    R: ResetPin,
{
    let pool_holds_a_job = async {
        while !serial.work_available().await {
            Timer::after(POOL_POLL_PERIOD).await;
        }
    };
    match select3(STOP_MINING.wait(), wait_for_halt(), pool_holds_a_job).await {
        Either3::Third(()) => InitPhase::Done,
        Either3::First(()) => {
            cut_rail(psu, chains).await;
            InitPhase::Stop
        }
        Either3::Second(()) => {
            cut_rail(psu, chains).await;
            InitPhase::Halted
        }
    }
}

/// How long no pool slot has held a job while mining.
#[derive(Default)]
struct PoolWatch {
    /// When the slots were first seen empty; `None` while a job is held.
    empty_since: Option<Instant>,
}

impl PoolWatch {
    /// Record one observation. Returns `true` once the slots have been
    /// empty for `grace` without a job in between.
    fn observe(&mut self, available: bool, now: Instant, grace: Duration) -> bool {
        if available {
            self.empty_since = None;
            return false;
        }
        let since = *self.empty_since.get_or_insert(now);
        now.saturating_duration_since(since) >= grace
    }

    /// Resolve once no pool slot has held a job for `POOL_LOSS_GRACE`.
    /// The future is dropped and rebuilt at every tuner step; the time
    /// already spent empty lives in `self`, so a short step cannot
    /// restart the count.
    async fn lost(&mut self, serial: &Serial) {
        loop {
            let available = serial.work_available().await;
            if self.observe(available, Instant::now(), POOL_LOSS_GRACE) {
                return;
            }
            Timer::after(POOL_POLL_PERIOD).await;
        }
    }
}

/// Cancellable settling sleep used between bring-up steps. STOP or
/// HALT raised during a 5-second hold must short-circuit so the rail
/// does not keep climbing toward `enable_PSU` after a thermal trip.
/// `Stop` and `Halt` outcomes drop the rail before returning so the
/// caller only needs to dispatch on the variant.
///
/// The PSU and the chain handles are two generic parameters of equal
/// standing, and neither is the receiver, the same shape as
/// [`cut_rail`], which this function calls on both abort paths.
async fn settle_or_abort<P, R>(
    duration: Duration,
    psu: &P,
    chains: &mut [ChainHandle<R>],
) -> InitPhase
where
    P: Psu,
    R: ResetPin,
{
    match select3(STOP_MINING.wait(), wait_for_halt(), Timer::after(duration)).await {
        Either3::Third(()) => InitPhase::Done,
        Either3::First(()) => {
            cut_rail(psu, chains).await;
            InitPhase::Stop
        }
        Either3::Second(()) => {
            cut_rail(psu, chains).await;
            InitPhase::Halted
        }
    }
}

/// Task shell that runs the [`Miner`] bring-up and teardown state
/// machine. Takes the board's PSU and reset-pin types, the descriptor
/// whose chip facts drive the tuner, the machine's chain count, its
/// tuner set and, when that set reads platform readings, the function
/// that reads them.
///
/// [`Miner::run`] returns only when `STOP_MINING` fires, which on bare
/// metal never happens. The shell parks afterwards for the same reason
/// `ubos_mining::serial_task` does.
#[macro_export]
macro_rules! miner_task {
    ($psu:ty, $reset:ty, $board:expr, $chains:expr, $tuner:ty) => {
        ubos_mining::miner_task!($psu, $reset, $board, $chains, $tuner, || ());
    };
    ($psu:ty, $reset:ty, $board:expr, $chains:expr, $tuner:ty, $platform:expr) => {
        #[embassy_executor::task]
        pub async fn miner_task(
            psu: &'static $psu,
            chains: [(&'static ubos_mining::serial::Serial, $reset); $chains],
        ) -> ! {
            /// Park interval after the state machine returns.
            const PARK_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(60);

            let mut miner = ubos_mining::Miner::<_, _, $tuner, { $chains }>::new(
                psu, $board, chains, $platform,
            );
            miner.run().await;
            defmt::info!("ubos-mining miner exited");
            loop {
                embassy_time::Timer::after(PARK_INTERVAL).await;
            }
        }
    };
}
