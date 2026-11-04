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
//! Live targets take effect at the next step. A stop or a halt allows the current
//! plan a bounded wait, then cuts the rail without a shutdown ramp.
//!
//! [`Miner::run`] is a state machine on the `transitions` crate: each state
//! is a type whose `run` does its work and returns the next state, and
//! `Miner::advance` runs one state at a time under the machine's only select.

mod error;
mod pool;

#[cfg(test)]
#[path = "tests/miner.rs"]
mod tests;

use core::cmp::Ordering;
use core::future::poll_fn;
use core::mem::{Discriminant, discriminant};
use core::pin::pin;
use core::task::Poll;

use embassy_futures::join::join_array;
use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use ubos_asic::HashboardDescriptor;
use ubos_config::{ConfigSubscription, Tuning};
use ubos_core::units::{Frequency, Temperature, Voltage};
use ubos_logging::{Debug2Format, error, info, warn};
use ubos_metrics::chain::ChainPresence;
use ubos_psu::Psu;

use crate::chain::{ChainControl, ChainHandle};
use crate::control::{Handle, Message, MiningState};
use crate::reset::ResetPin;
use crate::serial::Serial;
use crate::tuner::MachineTuner;
use transitions::{Lift, Machine, State, machine, successors};
use ubos_tuner::{ChainReadings, Plan, Readings};

pub use error::{MinerError, PsuErrorKind};
use pool::{POOL_POLL_PERIOD, PoolWatch};

/// Maximum time an interrupted hardware plan can finish before the rail cut.
const INTERRUPT_TIMEOUT: Duration = Duration::from_millis(100);

/// Plans in a row that may fail before the miner treats the tuner as
/// faulted and re-runs bring-up. A plan that writes to the hardware
/// clears the count; one that writes nothing, a wait or a value already
/// held, leaves it.
const PLAN_FAILURE_BUDGET: u8 = 3;
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

/// Request shutdown of the mining state machine from any state.
/// The rail is cut before [`Miner::run`] returns.
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

/// The ASIC mining state machine driving the injected chain driver.
///
/// Owns the PSU reference and every chain of the machine. Firmware
/// constructs this with [`Miner::new`] and calls [`Miner::run`] to
/// run the state machine. The `'a` lifetime covers the PSU reference
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
    /// Readings only this board's platform has. Read once per step and
    /// passed to the tuner.
    read_platform: fn() -> T::PlatformReadings,
    /// The mining handle the API reads; the miner publishes its lifecycle
    /// status and reads halt and resume through it.
    mining: Handle,
    /// Bring-up cycles that found no chain since the last one that did.
    /// Counts toward `BRING_UP_CYCLES`, and is cleared by every cycle
    /// that brings a chain up and by every resume.
    empty_cycles: u8,
}

impl<'a, P, R, T, const CHAINS: usize> Miner<'a, P, R, T, CHAINS>
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
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
            read_platform,
            mining: crate::handle(),
            empty_cycles: 0,
        }
    }

    /// Drive the mining state machine.
    ///
    /// Waits for the configuration task's first publication, cuts the
    /// rail, publishes the first state's lifecycle status, and then runs
    /// one state of [`MinerState`] after another through [`Self::advance`] from
    /// [`Choose`]. Returns when `STOP_MINING` fires; on bare metal
    /// that never happens, so the machine runs forever.
    ///
    /// The cut before the first state parks the rail and every chain, so
    /// each board begins bring-up from a known-off supply, whatever the
    /// previous boot cycle left it in.
    pub async fn run(&mut self) {
        let mut subscription = ubos_config::handle().subscribe();
        // This task is spawned before the configuration task, and the
        // first state reads the tuning. Without the wait it would read
        // `Config::default()`, the operating point of another board.
        subscription.changed().await;
        self.cut_rail().await;
        let mut state = MinerState::Choose(Choose);
        self.mining.publish(state.mining_state());
        while let Some(next) = self.advance(state, &mut subscription).await {
            state = next;
        }
    }

    /// Run `state` to its transition, cut the rail when the transition
    /// asks for it, and return the next state, or `None` when
    /// `STOP_MINING` ends the machine.
    ///
    /// The race runs first, and the cut and the status publication follow
    /// it, so nothing cancels the cut halfway. The status is published
    /// after the cut whenever the transition changes it.
    async fn advance(
        &mut self,
        state: MinerState<T>,
        config: &mut ConfigSubscription,
    ) -> Option<MinerState<T>> {
        let rail = state.rail();
        let status = state.mining_state();
        let next = self.race(state, config).await;
        let next_rail = next.as_ref().map(MinerState::rail);
        let leaves_live = rail == Rail::Live && next_rail != Some(Rail::Live);
        if leaves_live || next_rail == Some(Rail::Parked) {
            self.cut_rail().await;
        }
        let next_status = next
            .as_ref()
            .map_or(MiningState::Stopped, MinerState::mining_state);
        if next_status != status {
            self.mining.publish(next_status);
        }
        next
    }

    /// Run `state` against STOP and HALT, and return the next state, or
    /// `None` when STOP wins.
    ///
    /// This is the only select of the machine. STOP and HALT are read here
    /// for every state, and the state's own work decides every other
    /// transition. STOP wins over a HALT that is ready at the same time.
    /// [`Halted`] waits for the resume itself, so no message is read here
    /// while the machine is halted.
    ///
    /// A STOP or HALT that arrives while [`Step`] waits on the hardware
    /// gives the plan [`INTERRUPT_TIMEOUT`] to finish, so a rail write
    /// already on the bus completes before the cut. A STOP or HALT that is
    /// ready before [`Step`] starts ends it like any other state, so no new
    /// plan reaches the hardware after it. No message is read during that
    /// grace, so a later halt or resume stays queued.
    async fn race(
        &mut self,
        state: MinerState<T>,
        config: &mut ConfigSubscription,
    ) -> Option<MinerState<T>> {
        let reads_halt = state.reads_halt();
        let grants_grace = state.grants_grace();
        let mining = self.mining;
        let mut context = Context {
            miner: self,
            config,
        };
        let mut work = pin!(state.run(&mut context));
        let mut stop = pin!(STOP_MINING.wait());
        // A resume that arrives before a halt is dropped, because a
        // machine that is not halted has nothing to resume.
        let mut halt = pin!(async {
            if reads_halt {
                while !matches!(mining.next_message().await, Message::Halt) {}
            } else {
                core::future::pending::<()>().await;
            }
        });
        // Whether the work has waited on something, which a state that
        // writes to the hardware does from its first command on.
        let mut waited = false;
        let outcome = poll_fn(|cx| {
            if let Poll::Ready(()) = stop.as_mut().poll(cx) {
                return Poll::Ready(Either3::First(()));
            }
            if let Poll::Ready(()) = halt.as_mut().poll(cx) {
                return Poll::Ready(Either3::Second(()));
            }
            let polled = work.as_mut().poll(cx).map(Either3::Third);
            waited |= polled.is_pending();
            polled
        })
        .await;
        let (next, interrupted) = match outcome {
            Either3::First(()) => {
                info!("mining: stop signalled");
                (None, true)
            }
            Either3::Second(()) => {
                warn!("mining: halt asserted");
                (Some(Anywhere::Halt(Halted).lift()), true)
            }
            Either3::Third(next) => (Some(next), false),
        };
        if interrupted
            && grants_grace
            && waited
            && with_timeout(INTERRUPT_TIMEOUT, work.as_mut())
                .await
                .is_err()
        {
            warn!("mining: tuner cancellation timed out");
        }
        next
    }

    /// Count a bring-up cycle that brought no chain up, and decide
    /// between another cycle and a park.
    ///
    /// The rail cut is what a chain-level retry cannot do, and both
    /// failures that this budget exists for cleared on the cycle after
    /// one. `BRING_UP_CYCLES` bounds it: a machine whose
    /// connectors are empty parks in [`Halted`] instead of
    /// cutting its rail for as long as it is powered. A parked machine
    /// waits for the same resume a halt waits for. The manager sends one
    /// for a Start that finds nothing paused, so an operator sends
    /// Start, or reboots the board.
    fn no_chain_present(&mut self) -> BringUpNext {
        if !self.empty_cycle_spends_the_budget() {
            error!(
                "mining: no chain present on cycle {} of {}; cutting rail and retrying",
                self.empty_cycles, BRING_UP_CYCLES
            );
            return BringUpNext::Retry(Recover);
        }
        error!(
            "mining: no chain present in {} cycles; parking until resume",
            BRING_UP_CYCLES
        );
        crate::events::chains_parked(BRING_UP_CYCLES);
        BringUpNext::Park(Halted)
    }

    /// Count one bring-up cycle that brought no chain up, and report
    /// whether that spends `BRING_UP_CYCLES`.
    ///
    /// Separate from [`Self::no_chain_present`], and free of I/O, so
    /// the budget is testable without the bring-up around it.
    fn empty_cycle_spends_the_budget(&mut self) -> bool {
        self.empty_cycles = self.empty_cycles.saturating_add(1);
        self.empty_cycles >= BRING_UP_CYCLES
    }

    /// The actor that answers pool questions for the machine. Every
    /// chain shares one session, so chain 0 speaks for all of them, and
    /// the question touches no chip, so its own presence does not matter.
    fn pool_serial(&self) -> &'static Serial {
        self.chains[0].serial
    }

    /// Put every chain in reset and drop the rail.
    ///
    /// The order matters. Reset is asserted before the rail goes, so the
    /// chips are held rather than losing power mid-transaction. A failure to
    /// disable the PSU is logged; the chips remain in reset.
    ///
    /// A chain that was present is recorded as `Unknown` here: its
    /// enumeration is void once the reset is asserted, and the next
    /// bring-up decides it again. An absent or faulted chain keeps its
    /// verdict, so a parked connector still reads as parked while the
    /// machine is down. The store is a direct write rather than an actor
    /// command, so a cut never waits behind a bring-up command the actor
    /// is still running.
    async fn cut_rail(&mut self) {
        for chain in &mut self.chains {
            chain.reset.enter_reset().await;
            if chain.is_present() {
                chain.stats.set_presence(ChainPresence::Unknown);
            }
        }
        if let Err(error) = self.psu.disable().await {
            warn!("psu disable failed: {:?}", Debug2Format(&error));
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
    /// The setpoint is present by construction. `start_chains` commands
    /// the open-core rail and [`StartWork`] leads to
    /// [`Step`] only after that command succeeded, and
    /// `Psu::set_voltage` records a successful write before it returns.
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

/// What a state runs against: the miner, and the configuration
/// subscription the state reads the tuning through. The subscription is
/// a borrow, so a test drives the machine from a configuration of its
/// own.
struct Context<'m, 'a, P, R, T, const CHAINS: usize>
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    miner: &'m mut Miner<'a, P, R, T, CHAINS>,
    config: &'m mut ConfigSubscription,
}

machine! {
    /// The state the miner is in.
    ///
    /// `Choose`, `Recover`, `NoTuner` and `Halted` hold the rail off, and
    /// `Reset` and `WaitForPool` run before `Enable` raises it. Every state
    /// but the first four counts as powered, and leaving one for those four
    /// cuts the rail.
    enum MinerState<T> {
        Choose(Choose),
        Reset(Reset),
        WaitForPool(WaitForPool),
        Enable(Enable),
        ExitReset(ExitReset),
        BringUp(BringUp),
        StartWork(StartWork),
        Step(Step<T>),
        Pause(Pause<T>),
        Recover(Recover),
        NoTuner(NoTuner),
        Halted(Halted),
    }
    anywhere = Anywhere;
}

successors! {
    /// The transitions that interrupt every state.
    enum Anywhere { Halt(Halted) }
}

/// What a state does with the rail.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rail {
    /// The rail is up or about to be raised, so leaving for a state that
    /// holds it off cuts it. [`Reset`] and [`WaitForPool`] are this with
    /// the rail still off, so a halt or a stop during the pool wait cuts
    /// it again.
    Live,
    /// The state holds the rail off.
    Off,
    /// The state holds the rail off for as long as it takes, and entering
    /// it cuts the rail whatever state came before.
    ///
    /// [`NoTuner`] and [`Halted`] can hold indefinitely on the first pass
    /// after boot, where the only cut so far is the one in [`Miner::run`],
    /// and that cut only logs a failed disable. Cutting again on entry
    /// means a board that boots under an unusable tuning or a queued halt
    /// never holds with a rail that nobody confirmed down.
    Parked,
}

impl<T> MinerState<T> {
    /// What this state does with the rail.
    fn rail(&self) -> Rail {
        match self {
            Self::Reset(_)
            | Self::WaitForPool(_)
            | Self::Enable(_)
            | Self::ExitReset(_)
            | Self::BringUp(_)
            | Self::StartWork(_)
            | Self::Step(_)
            | Self::Pause(_) => Rail::Live,
            Self::Choose(_) | Self::Recover(_) => Rail::Off,
            Self::NoTuner(_) | Self::Halted(_) => Rail::Parked,
        }
    }

    /// Whether the select reads HALT while this state runs. [`Halted`]
    /// reads the messages itself.
    fn reads_halt(&self) -> bool {
        !matches!(self, Self::Halted(_))
    }

    /// Whether a STOP or HALT lets this state finish the command it waits
    /// on before the cut. Only [`Step`] writes plans to the hardware.
    fn grants_grace(&self) -> bool {
        matches!(self, Self::Step(_))
    }

    /// The lifecycle status the API and the metrics report for this state.
    fn mining_state(&self) -> MiningState {
        match self {
            Self::Reset(_) | Self::WaitForPool(_) => MiningState::WaitingForPool,
            Self::Enable(_) | Self::ExitReset(_) | Self::BringUp(_) | Self::StartWork(_) => {
                MiningState::Starting
            }
            Self::Step(_) | Self::Pause(_) => MiningState::Running,
            Self::Choose(_) | Self::Recover(_) | Self::NoTuner(_) | Self::Halted(_) => {
                MiningState::Stopped
            }
        }
    }
}

/// Check that this machine has a tuner for the configured tuning.
struct Choose;

successors! { enum ChooseNext { Missing(NoTuner), Found(Reset) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Choose
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = ChooseNext;

    /// Check the tuner before any chain is powered. A configuration that
    /// selects a tuner this machine does not have leads to [`NoTuner`],
    /// with the rail left off.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> ChooseNext {
        let tuning = context.config.get().await.miner.tuning;
        if T::select(&tuning, context.miner.board).is_none() {
            return ChooseNext::Missing(NoTuner::missing(&tuning));
        }
        ChooseNext::Found(Reset)
    }
}

/// Put every chain in reset.
struct Reset;

successors! { enum ResetNext { Held(WaitForPool) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Reset
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = ResetNext;

    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> ResetNext {
        info!("mining: entering reset");
        crate::events::miner_reset();
        for chain in &mut context.miner.chains {
            chain.reset.enter_reset().await;
        }
        ResetNext::Held(WaitForPool)
    }
}

/// Hold every chain in reset until a pool slot holds a job.
struct WaitForPool;

successors! { enum WaitForPoolNext { Job(Enable) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for WaitForPool
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = WaitForPoolNext;

    /// Hold the chains in reset with the rail off until a pool slot holds
    /// a job, asking every [`POOL_POLL_PERIOD`].
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> WaitForPoolNext {
        info!("mining: waiting for a pool to hold a job");
        while !context.miner.pool_serial().work_available().await {
            Timer::after(POOL_POLL_PERIOD).await;
        }
        crate::events::pool_available();
        WaitForPoolNext::Job(Enable)
    }
}

/// Hold reset a little longer, then enable the rail.
struct Enable;

successors! { enum EnableNext { Enabled(ExitReset) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Enable
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = EnableNext;

    /// Hold the chains in reset for [`RESET_HOLD`], then enable the
    /// rail and let it settle for [`RESET_HOLD`]. A failure is
    /// logged and bring-up goes on, because enumeration finds a rail that
    /// did not come up.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> EnableNext {
        Timer::after(RESET_HOLD).await;
        info!("mining: enabling PSU");
        crate::events::psu_enabled();
        if let Err(error) = context.miner.psu.enable().await {
            warn!("psu enable failed: {:?}", Debug2Format(&error));
        }
        Timer::after(RESET_HOLD).await;
        EnableNext::Enabled(ExitReset)
    }
}

/// Release every chain from reset.
struct ExitReset;

successors! { enum ExitResetNext { Released(BringUp) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for ExitReset
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = ExitResetNext;

    /// Release every chain from reset and let it settle for
    /// [`RESET_HOLD`].
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> ExitResetNext {
        info!("mining: exiting reset");
        for chain in &mut context.miner.chains {
            chain.reset.exit_reset().await;
        }
        Timer::after(RESET_HOLD).await;
        ExitResetNext::Released(BringUp)
    }
}

/// Enumerate and configure every chain.
struct BringUp;

successors! { enum BringUpNext { Present(StartWork), Retry(Recover), Park(Halted) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for BringUp
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = BringUpNext;

    /// Reset, enumerate and configure every chain, all at once. This is
    /// the last bring-up step before the tuner sees a chain.
    ///
    /// What one chain does is [`ChainHandle::bring_up`]. A chain whose
    /// count never settles on the board's chip count within the retry
    /// budget is marked absent when its last answer was no chips at all,
    /// and faulted when it was a count, a sweep no chip answered, or a
    /// refused safe-boot configuration. Either way the chain is parked in
    /// reset and left out of the ramp, and the other chains carry on:
    /// only a bring-up where no chain comes up present cuts the rail.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> BringUpNext {
        let miner = &mut *context.miner;
        let expected = miner.board.num_chips;
        join_array(
            miner
                .chains
                .each_mut()
                .map(|chain| chain.bring_up(expected)),
        )
        .await;
        if miner.chains.iter().any(ChainHandle::is_present) {
            miner.empty_cycles = 0;
            return BringUpNext::Present(StartWork);
        }
        miner.no_chain_present()
    }
}

/// Build the tuner, anchor the rail and start work on every present
/// chain.
struct StartWork;

successors! {
    enum StartWorkNext<T> { Missing(NoTuner), Failed(Recover), Started(Step<T>) }
}

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for StartWork
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = StartWorkNext<T>;

    /// Build the tuner the configuration selects now, anchor the shared
    /// rail and start every present chain.
    ///
    /// Building the tuner at the end of bring-up means that a tuning that
    /// changed to another tuner this machine has during bring-up is the
    /// one that runs, without another power cycle. A tuning that changed
    /// to one this machine does not have leads to [`NoTuner`], which cuts
    /// the rail.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> StartWorkNext<T> {
        let tuning = context.config.get().await.miner.tuning;
        let miner = &mut *context.miner;
        let Some(tuner) = T::select(&tuning, miner.board) else {
            return StartWorkNext::Missing(NoTuner::missing(&tuning));
        };
        info!("mining: running work initialization");
        if let Err(error) = miner.start_chains().await {
            error!(
                "mining: work initialization error ({:?}); cutting rail and retrying",
                Debug2Format(&error)
            );
            crate::events::work_initialization_failed();
            return StartWorkNext::Failed(Recover);
        }
        crate::events::work_initialized();
        // Every present chain is enumerated, configured and clocked;
        // from here it only hashes. A board that addresses it
        // differently for the two moves its mode line now.
        for chain in &mut miner.chains {
            if chain.is_present() {
                chain.reset.enter_work_mode().await;
            }
        }
        StartWorkNext::Started(Step {
            run: Run {
                tuner,
                selected: Selected::of(&tuning),
                failures: 0,
                pool: PoolWatch::default(),
            },
        })
    }
}

/// Run one tuner step and apply its plan.
struct Step<T> {
    run: Run<T>,
}

successors! {
    enum StepNext<T> { Superseded(Choose), Faulted(Recover), Applied(Pause<T>) }
}

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Step<T>
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = StepNext<T>;

    /// Run one tuner step against the live target and apply its plan.
    ///
    /// A configuration that selects another tuner leads to [`Choose`]
    /// before the step. A plan that writes to the hardware clears
    /// `failures`; a plan that fails adds one, and the
    /// [`PLAN_FAILURE_BUDGET`]th failure in a row leads to [`Recover`].
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> StepNext<T> {
        let mut run = self.run;
        let snapshot = context.config.get().await;
        if !run.selected.serves(&snapshot.miner.tuning) {
            return StepNext::Superseded(Choose);
        }
        let miner = &*context.miner;
        let plan = run.tuner.step(
            &snapshot.miner.tuning,
            &miner.readings(),
            &(miner.read_platform)(),
        );
        match miner.apply_plan(&plan).await {
            Ok(true) => run.failures = 0,
            Ok(false) => {}
            Err(error) => {
                run.failures += 1;
                warn!(
                    "mining: plan failed ({} of {}): {:?}",
                    run.failures,
                    PLAN_FAILURE_BUDGET,
                    Debug2Format(&error)
                );
                if run.failures >= PLAN_FAILURE_BUDGET {
                    crate::events::tuner_fault();
                    return StepNext::Faulted(Recover);
                }
            }
        }
        StepNext::Applied(Pause {
            run,
            until: Instant::now() + plan.delay,
        })
    }
}

/// Wait for the delay the last plan asked for.
struct Pause<T> {
    run: Run<T>,
    /// When the delay ends.
    until: Instant,
}

successors! {
    enum PauseNext<T> { Elapsed(Step<T>), Superseded(Choose), PoolLost(Choose) }
}

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Pause<T>
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = PauseNext<T>;

    /// Wait until `until`.
    ///
    /// A configuration that selects another tuner ends the wait and leads
    /// to [`Choose`]; a new target for the running tuner does not, and
    /// reaches the tuner at the next step. No pool slot holding a job for
    /// [`POOL_LOSS_GRACE`] also leads to [`Choose`], which waits for a
    /// pool again with the rail off.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> PauseNext<T> {
        let Self { mut run, until } = self;
        match select3(
            Timer::at(until),
            run.selected.superseded(context.config),
            run.pool.lost(context.miner.pool_serial()),
        )
        .await
        {
            Either3::First(()) => PauseNext::Elapsed(Step { run }),
            Either3::Second(()) => PauseNext::Superseded(Choose),
            Either3::Third(()) => {
                warn!("mining: no pool has held a job for the grace period; cutting rail");
                crate::events::pool_lost();
                PauseNext::PoolLost(Choose)
            }
        }
    }
}

/// Hold the rail off before the next bring-up.
struct Recover;

successors! { enum RecoverNext { Retry(Choose) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Recover
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = RecoverNext;

    /// Hold the rail off for [`RETRY_DELAY`] after a failed
    /// bring-up or a faulted tuner, then start over.
    async fn run(self, _context: &mut Context<'_, '_, P, R, T, CHAINS>) -> RecoverNext {
        Timer::after(RETRY_DELAY).await;
        RecoverNext::Retry(Choose)
    }
}

/// Hold the rail off until the tuning variant changes, because this machine
/// has no tuner for the configured tuning.
struct NoTuner {
    /// The tuning variant that has no tuner here.
    selected: Selected,
}

impl NoTuner {
    /// The state for a configuration that selects `tuning`, which this
    /// machine has no tuner for.
    fn missing(tuning: &Tuning) -> Self {
        error!("mining: this machine has no tuner for the configured tuning");
        Self {
            selected: Selected::of(tuning),
        }
    }
}

successors! { enum NoTunerNext { Configured(Choose) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for NoTuner
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = NoTunerNext;

    /// Hold the rail off until a configuration selects another tuning
    /// variant, which [`Choose`] checks again.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> NoTunerNext {
        self.selected.superseded(context.config).await;
        NoTunerNext::Configured(Choose)
    }
}

/// Hold the rail off until a resume, after a halt or after a park.
struct Halted;

successors! { enum HaltedNext { Resume(Choose) } }

impl<P, R, T, const CHAINS: usize> State<Context<'_, '_, P, R, T, CHAINS>> for Halted
where
    P: Psu,
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = HaltedNext;

    /// Wait for a resume, dropping every halt before it, and start over.
    ///
    /// A resumed machine has been asked for the whole budget of empty
    /// bring-up cycles again, so the count starts from zero.
    async fn run(self, context: &mut Context<'_, '_, P, R, T, CHAINS>) -> HaltedNext {
        info!("mining: rail down; awaiting resume");
        while !matches!(context.miner.mining.next_message().await, Message::Resume) {}
        info!("mining: resumed; restarting bring-up");
        context.miner.empty_cycles = 0;
        HaltedNext::Resume(Choose)
    }
}

/// What a mining run carries from step to step. [`StartWork`] builds it,
/// and the run ends when it is dropped.
struct Run<T> {
    tuner: T,
    /// The tuning variant `tuner` was built from.
    selected: Selected,
    /// The plans in a row that failed.
    failures: u8,
    /// How long no pool slot has held a job.
    pool: PoolWatch,
}

/// A tuning variant, which stands for the tuner built from it. A new
/// target within the variant reaches the running tuner at its next step,
/// and another variant needs another tuner.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Selected(Discriminant<Tuning>);

impl Selected {
    fn of(tuning: &Tuning) -> Self {
        Self(discriminant(tuning))
    }

    /// Whether `tuning` is of this variant.
    fn serves(self, tuning: &Tuning) -> bool {
        self == Self::of(tuning)
    }

    /// Wait for a configuration that selects another variant.
    async fn superseded(self, subscription: &mut ConfigSubscription) {
        while self.serves(&subscription.changed().await.miner.tuning) {}
    }
}

/// Task shell that runs the [`Miner`]. Takes the board's PSU and reset-pin
/// types, the descriptor whose chip facts drive the tuner, the machine's
/// chain count, its tuner set and, when that set reads platform readings,
/// the function that reads them.
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
