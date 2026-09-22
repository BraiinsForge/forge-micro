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
//! Bring-up configures every chain concurrently and parks absent or faulted connectors.
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
use crate::control::{COMMAND_CHANNEL, Message, MiningState, publish_state};
use crate::reset::ResetPin;
use crate::serial::Serial;
use crate::tuner::MachineTuner;
use crate::tuner::fixed::FixedTuner;
use ubos_tuner::{ChainReadings, Plan, Readings};

/// Maximum time an interrupted hardware plan can finish before the rail cut.
const INTERRUPT_TIMEOUT: Duration = Duration::from_millis(100);

/// Request shutdown during initialization, plan execution or settling.
/// Teardown cuts the rail before the outer loop returns.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub static STOP_MINING: Signal<CriticalSectionRawMutex, ()> = Signal::new();

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
/// which today is always `'static` at the firmware call site.
pub struct Miner<'a, P, R, const CHAINS: usize>
where
    P: Psu,
    R: ResetPin,
{
    psu: &'a P,
    board: &'static HashboardDescriptor,
    chains: [ChainHandle<R>; CHAINS],
    halted: bool,
}

impl<'a, P, R, const CHAINS: usize> Miner<'a, P, R, CHAINS>
where
    P: Psu,
    R: ResetPin,
{
    /// Settling time held between each reset / PSU transition during
    /// the bring-up sequence.
    const RESET_HOLD: Duration = Duration::from_secs(5);

    /// Pause between a failed bring-up and the next attempt. Long enough
    /// for the chip's rail to fully discharge and any transient cable /
    /// board glitch to clear.
    const RETRY_DELAY: Duration = Duration::from_secs(5);

    /// Construct a `Miner` from a PSU reference, the board descriptor
    /// whose chip facts drive the tuner, and one serial actor with its
    /// reset pin per chain of the machine, in chain order.
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
            let chosen = <FixedTuner as MachineTuner<CHAINS>>::select(&tuning, self.board);
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
    /// A chain that answers zero chips is marked absent; one whose
    /// count never settles on `self.board.num_chips`, or that answers
    /// the count and then refuses its safe-boot configuration, within
    /// the retry budget is marked faulted. Either way the chain is
    /// parked in reset and left out of the ramp, and the other chains
    /// carry on: only a bring-up where no chain comes up present cuts
    /// the rail.
    async fn bring_up_chains_phase(&mut self) -> InitPhase {
        let expected = self.board.num_chips;
        match select3(
            STOP_MINING.wait(),
            wait_for_halt(),
            bring_up_all(&mut self.chains, expected),
        )
        .await
        {
            Either3::Third(BringUpOutcome::Done) => InitPhase::Done,
            Either3::Third(BringUpOutcome::NonePresent) => {
                error!("mining: no chain present; cutting rail and retrying");
                self.cut_rail_and_settle().await
            }
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
        let init_result = select3(STOP_MINING.wait(), wait_for_halt(), self.start_work()).await;
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
    async fn bring_up_cycle<M: MachineTuner<CHAINS>>(
        &mut self,
        tuner: &mut M,
        config_subscription: &mut ConfigSubscription,
        selected: Discriminant<Tuning>,
    ) -> CycleOutcome {
        if self.halted {
            info!("mining: halt active; awaiting clear");
            match wait_halt_clear_or_stop().await {
                Resumed::Cleared => {
                    info!("mining: halt cleared; restarting bring-up");
                    self.halted = false;
                }
                Resumed::Stopped => return CycleOutcome::Stop,
            }
        }

        publish_state(MiningState::Starting);
        info!("mining: entering reset");
        crate::events::miner_reset();
        for chain in &mut self.chains {
            chain.reset.enter_reset().await;
        }
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

        publish_state(MiningState::Running);
        self.mine_phase(tuner, config_subscription, selected).await
    }

    /// Read the live target at each step and retain the first accepted interruption.
    async fn mine_phase<M: MachineTuner<CHAINS>>(
        &mut self,
        tuner: &mut M,
        subscription: &mut ConfigSubscription,
        selected: Discriminant<Tuning>,
    ) -> CycleOutcome {
        let mut plan_delay = Duration::from_millis(0);
        let interrupt = loop {
            if let Either::First(exit) = select(
                interrupted(selected, subscription),
                Timer::after(plan_delay),
            )
            .await
            {
                break exit;
            }
            let snapshot = subscription.get().await;
            if discriminant(&snapshot.miner.tuning) != selected {
                break Interrupt::Restart;
            }
            let plan = tuner.step(&snapshot.miner.tuning, &self.readings());
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
                    warn!("mining: plan failed: {:?}", Debug2Format(&error));
                    // TODO: Define a retry policy for transient plan failures.
                    crate::events::tuner_fault();
                    let phase = self.cut_rail_and_settle().await;
                    // `cut_rail_and_settle` never returns `Done`, so the
                    // fallback stands only to keep the expression total.
                    return self.leave_cycle(phase).unwrap_or(CycleOutcome::Continue);
                }
                Either::Second(Err(error)) => warn!(
                    "mining: interrupted plan failed: {:?}",
                    Debug2Format(&error)
                ),
                Either::Second(Ok(())) => {}
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

    /// Decide what `bring_up_cycle` does after one bring-up step:
    /// `None` to go on with the next step, `Some` to leave the cycle
    /// with that outcome. A halt edge records the new halt state on
    /// the way out, so the next cycle waits for the clear first.
    fn leave_cycle(&mut self, phase: InitPhase) -> Option<CycleOutcome> {
        match phase {
            InitPhase::Done => None,
            InitPhase::Stop => Some(CycleOutcome::Stop),
            InitPhase::Halted => {
                self.halted = true;
                Some(CycleOutcome::Continue)
            }
            InitPhase::Retry => Some(CycleOutcome::Continue),
        }
    }

    /// Set the open-core rail and start work only on configured chains.
    async fn start_work(&self) -> Result<(), MinerError> {
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
    /// The setpoint is present by construction. `start_work` commands the
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
    async fn apply_plan(&self, plan: &Plan<CHAINS>) -> Result<(), MinerError> {
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

        self.set_frequencies(decreases).await?;
        // Only the ceiling is clamped here. It is board policy that can sit
        // above the PSU's own range, so nothing below would refuse it. The
        // floor is the PSU's, and a rail under its range returns
        // `Error::OutOfRange` rather than reaching the hardware.
        self.set_voltage_if_different(
            plan.rail
                .map(|rail| rail.min(self.board.max_rail_voltage())),
        )
        .await?;
        self.set_frequencies(increases).await?;
        Ok(())
    }

    async fn set_frequencies(
        &self,
        frequencies: [Option<Frequency>; CHAINS],
    ) -> Result<(), MinerError> {
        for (chain, next) in self.chains.iter().zip(frequencies) {
            if let Some(next) = next {
                chain.set_frequency(next).await?;
            }
        }
        Ok(())
    }

    async fn set_voltage_if_different(&self, voltage: Option<Voltage>) -> Result<(), MinerError> {
        let Some(voltage) =
            voltage.filter(|next| Some(*next) != ubos_psu::handle().commanded_voltage())
        else {
            return Ok(());
        };
        self.psu
            .set_voltage(voltage)
            .await
            .map_err(|error| MinerError::Psu(error.into()))?;
        Ok(())
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

/// Idle counterpart to [`Miner::run`] for deployments without a real chip.
/// Publish the stopped state and drain commands without hardware access.
pub async fn run_idle() -> ! {
    publish_state(MiningState::Stopped);
    info!("mining: idle loop active (no chip)");
    loop {
        next_halt_change().await;
    }
}

async fn next_halt_change() -> bool {
    match COMMAND_CHANNEL.receive().await {
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
    publish_state(MiningState::Stopped);
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
    /// A transient failure that has already cut the rail; the cycle
    /// restarts from a fresh config snapshot.
    Retry,
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
/// whose chip facts drive the tuner, and the machine's chain count.
///
/// [`Miner::run`] returns only when `STOP_MINING` fires, which on bare
/// metal never happens. The shell parks afterwards for the same reason
/// `ubos_mining::serial_task` does.
#[macro_export]
macro_rules! miner_task {
    ($psu:ty, $reset:ty, $board:expr, $chains:expr) => {
        #[embassy_executor::task]
        pub async fn miner_task(
            psu: &'static $psu,
            chains: [(&'static ubos_mining::serial::Serial, $reset); $chains],
        ) -> ! {
            /// Park interval after the state machine returns.
            const PARK_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(60);

            let mut miner = ubos_mining::Miner::new(psu, $board, chains);
            miner.run().await;
            defmt::info!("ubos-mining miner exited");
            loop {
                embassy_time::Timer::after(PARK_INTERVAL).await;
            }
        }
    };
}
