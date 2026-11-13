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

//! The states of the miner: each is a type whose `run` does the work of the
//! state and returns the next state.

use core::mem::{Discriminant, discriminant};

use embassy_futures::join::join_array;
use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Instant, Timer};
use transitions::{State, successors};
use ubos_config::{ConfigSubscription, Tuning};
use ubos_logging::{Debug2Format, error, info, warn};

use super::BRING_UP_CYCLES;
use super::machine::Context;
use super::pool::{POOL_POLL_PERIOD, PoolWatch};
use crate::chain::ChainHandle;
use crate::reset::ResetPin;
use crate::tuner::MachineTuner;

/// Plans in a row that may fail before the miner treats the tuner as
/// faulted and re-runs bring-up. A plan that writes to the hardware
/// clears the count; one that writes nothing, a wait or a value already
/// held, leaves it.
pub(super) const PLAN_FAILURE_BUDGET: u8 = 3;

/// Settling time held between each reset / PSU transition during
/// the bring-up sequence.
const RESET_HOLD: Duration = Duration::from_secs(5);

/// Pause between a failed bring-up and the next attempt. Long enough
/// for the chip's rail to fully discharge and any transient cable /
/// board glitch to clear.
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Check that this machine has a tuner for the configured tuning.
pub(super) struct Choose;

successors! { pub(super) enum ChooseNext { Missing(NoTuner), Found(Reset) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Choose
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = ChooseNext;

    /// Check the tuner before any chain is powered. A configuration that
    /// selects a tuner this machine does not have leads to [`NoTuner`],
    /// with the rail left off.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> ChooseNext {
        let tuning = context.config.get().await.miner.tuning;
        if T::select(&tuning, context.miner.board).is_none() {
            return ChooseNext::Missing(NoTuner::missing(&tuning));
        }
        ChooseNext::Found(Reset)
    }
}

/// Put every chain in reset.
pub(super) struct Reset;

successors! { pub(super) enum ResetNext { Held(WaitForPool) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Reset
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = ResetNext;

    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> ResetNext {
        info!("mining: entering reset");
        crate::events::miner_reset();
        for chain in &mut context.miner.chains {
            chain.reset.enter_reset().await;
        }
        ResetNext::Held(WaitForPool)
    }
}

/// Hold every chain in reset until the scheduler would dispatch work.
pub(super) struct WaitForPool;

successors! { pub(super) enum WaitForPoolNext { Job(Enable) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for WaitForPool
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = WaitForPoolNext;

    /// Hold the chains in reset with the rail off until the scheduler would
    /// dispatch work, asking every [`POOL_POLL_PERIOD`].
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> WaitForPoolNext {
        info!("mining: waiting for a pool to hold a job");
        while !context.miner.pool_serial().work_available().await {
            Timer::after(POOL_POLL_PERIOD).await;
        }
        crate::events::pool_available();
        WaitForPoolNext::Job(Enable)
    }
}

/// Hold reset a little longer, then enable the rail.
pub(super) struct Enable;

successors! { pub(super) enum EnableNext { Enabled(ExitReset) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Enable
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = EnableNext;

    /// Hold the chains in reset for [`RESET_HOLD`], then enable the
    /// rail and let it settle for [`RESET_HOLD`]. A failure is
    /// logged and bring-up goes on, because enumeration finds a rail that
    /// did not come up.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> EnableNext {
        Timer::after(RESET_HOLD).await;
        info!("mining: enabling PSU");
        crate::events::psu_enabled();
        if let Err(error) = context.miner.psu_handle.enable().await {
            warn!("psu enable failed: {:?}", Debug2Format(&error));
        }
        Timer::after(RESET_HOLD).await;
        EnableNext::Enabled(ExitReset)
    }
}

/// Release every chain from reset.
pub(super) struct ExitReset;

successors! { pub(super) enum ExitResetNext { Released(BringUp) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for ExitReset
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = ExitResetNext;

    /// Release every chain from reset and let it settle for
    /// [`RESET_HOLD`].
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> ExitResetNext {
        info!("mining: exiting reset");
        for chain in &mut context.miner.chains {
            chain.reset.exit_reset().await;
        }
        Timer::after(RESET_HOLD).await;
        ExitResetNext::Released(BringUp)
    }
}

/// Enumerate and configure every chain.
pub(super) struct BringUp;

successors! { pub(super) enum BringUpNext { Present(StartWork), Retry(Recover), Park(Halted) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for BringUp
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = BringUpNext;

    /// Reset, enumerate and configure every chain, all at once. This is
    /// the last bring-up step before the tuner sees a chain.
    ///
    /// A bring-up that finds no chain counts against `BRING_UP_CYCLES`. The
    /// rail cut is what a chain-level retry cannot do, and both failures
    /// that this budget exists for cleared on the cycle after one. A
    /// machine whose connectors are empty parks in [`Halted`] instead of
    /// cutting its rail for as long as it is powered. A parked machine
    /// waits for the same resume a halt waits for. The manager sends one
    /// for a Start that finds nothing paused, so an operator sends Start,
    /// or reboots the board.
    ///
    /// What one chain does is [`ChainHandle::bring_up`]. A chain whose
    /// count never settles on the board's chip count within the retry
    /// budget is marked absent when its last answer was no chips at all,
    /// and faulted when it was a count, a sweep no chip answered, or a
    /// refused safe-boot configuration. Either way the chain is parked in
    /// reset and left out of the ramp, and the other chains carry on:
    /// only a bring-up where no chain comes up present cuts the rail.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> BringUpNext {
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
        if !miner.empty_cycle_spends_the_budget() {
            error!(
                "mining: no chain present on cycle {} of {}; cutting rail and retrying",
                miner.empty_cycles, BRING_UP_CYCLES
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
}

/// Build the tuner, anchor the rail and start work on every present
/// chain.
pub(super) struct StartWork;

successors! {
    pub(super) enum StartWorkNext<T> { Missing(NoTuner), Failed(Recover), Started(Step<T>) }
}

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for StartWork
where
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
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> StartWorkNext<T> {
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
pub(super) struct Step<T> {
    pub(super) run: Run<T>,
}

successors! {
    pub(super) enum StepNext<T> { Superseded(Choose), Faulted(Recover), Applied(Pause<T>) }
}

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Step<T>
where
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
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> StepNext<T> {
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
pub(super) struct Pause<T> {
    pub(super) run: Run<T>,
    /// When the delay ends.
    pub(super) until: Instant,
}

successors! {
    pub(super) enum PauseNext<T> { Elapsed(Step<T>), Superseded(Choose), PoolLost(Choose) }
}

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Pause<T>
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = PauseNext<T>;

    /// Wait until `until`.
    ///
    /// A configuration that selects another tuner ends the wait and leads
    /// to [`Choose`]; a new target for the running tuner does not, and
    /// reaches the tuner at the next step. A scheduler with nothing to dispatch for
    /// [`POOL_LOSS_GRACE`] also leads to [`Choose`], which waits for a
    /// pool again with the rail off.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> PauseNext<T> {
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
pub(super) struct Recover;

successors! { pub(super) enum RecoverNext { Retry(Choose) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Recover
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = RecoverNext;

    /// Hold the rail off for [`RETRY_DELAY`] after a failed
    /// bring-up or a faulted tuner, then start over.
    async fn run(self, _context: &mut Context<'_, R, T, CHAINS>) -> RecoverNext {
        Timer::after(RETRY_DELAY).await;
        RecoverNext::Retry(Choose)
    }
}

/// Hold the rail off until the tuning variant changes, because this machine
/// has no tuner for the configured tuning.
pub(super) struct NoTuner {
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

successors! { pub(super) enum NoTunerNext { Configured(Choose) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for NoTuner
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = NoTunerNext;

    /// Hold the rail off until a configuration selects another tuning
    /// variant, which [`Choose`] checks again.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> NoTunerNext {
        self.selected.superseded(context.config).await;
        NoTunerNext::Configured(Choose)
    }
}

/// Hold the rail off until a resume, after a halt or after a park.
pub(super) struct Halted;

successors! { pub(super) enum HaltedNext { Resume(Choose) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Halted
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = HaltedNext;

    /// Wait for a resume, dropping every halt before it, and start over.
    ///
    /// A resumed machine has been asked for the whole budget of empty
    /// bring-up cycles again, so the count starts from zero.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> HaltedNext {
        info!("mining: rail down; awaiting resume");
        context.miner.mining.resumed().await;
        info!("mining: resumed; restarting bring-up");
        context.miner.empty_cycles = 0;
        HaltedNext::Resume(Choose)
    }
}

/// What a mining run carries from step to step. [`StartWork`] builds it,
/// and the run ends when it is dropped.
pub(super) struct Run<T> {
    pub(super) tuner: T,
    /// The tuning variant `tuner` was built from.
    pub(super) selected: Selected,
    /// The plans in a row that failed.
    pub(super) failures: u8,
    /// How long the scheduler has had nothing to dispatch.
    pub(super) pool: PoolWatch,
}

/// A tuning variant, which stands for the tuner built from it. A new
/// target within the variant reaches the running tuner at its next step,
/// and another variant needs another tuner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Selected(Discriminant<Tuning>);

impl Selected {
    pub(super) fn of(tuning: &Tuning) -> Self {
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
