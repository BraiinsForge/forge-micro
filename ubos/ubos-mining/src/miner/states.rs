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
use embassy_futures::select::{Either, Either4, select, select4};
use embassy_time::{Duration, Instant, Timer};
use transitions::{State, successors};
use ubos_config::{ConfigSubscription, Tuning};
use ubos_logging::{Debug2Format, error, info, warn};

use super::BRING_UP_CYCLES;
use super::machine::Context;
use super::pool::{POOL_POLL_PERIOD, PoolWatch};
use super::temperature::{TemperatureWatch, danger_cleared, unreadable_grace};
use crate::chain::ChainHandle;
use crate::control::Message;
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

    /// A failed enable is logged and bring-up goes on, because enumeration
    /// finds a rail that did not come up.
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

    /// Only a bring-up where no chain comes up present cuts the rail; one
    /// chain's verdict is [`ChainHandle::bring_up`]'s. Such a bring-up
    /// counts against `BRING_UP_CYCLES`, and a spent budget parks the
    /// machine in [`Halted`], which waits for the resume the manager sends
    /// for a Start that finds nothing paused.
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
            return BringUpNext::Retry(Recover::after_fault());
        }
        error!(
            "mining: no chain present in {} cycles; parking until resume",
            BRING_UP_CYCLES
        );
        crate::events::chains_parked(BRING_UP_CYCLES);
        BringUpNext::Park(Halted::until_resume())
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

    /// The tuner is built at the end of bring-up, so a tuning that changed
    /// during bring-up to another tuner this machine has is the one that
    /// runs, without another power cycle.
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
            return StartWorkNext::Failed(Recover::after_fault());
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
                temperature: TemperatureWatch::default(),
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

    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> StepNext<T> {
        let mut run = self.run;
        let snapshot = context.config.get().await;
        if !run.selected.serves(&snapshot.miner.tuning) {
            return StepNext::Superseded(Choose);
        }
        let miner = &*context.miner;
        let plan = run.tuner.step(
            &snapshot.miner.tuning,
            &miner.readings(snapshot.thermal.dangerous.chip),
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
                    return StepNext::Faulted(Recover::after_fault());
                }
            }
        }
        StepNext::Applied(Pause {
            run,
            until: Instant::now() + plan.delay,
            unreadable_grace: unreadable_grace(snapshot.thermal.sample_interval_ms),
        })
    }
}

/// Wait for the delay the last plan asked for.
pub(super) struct Pause<T> {
    pub(super) run: Run<T>,
    pub(super) until: Instant,
    /// How long thermal may have no reading before the rail is cut.
    pub(super) unreadable_grace: Duration,
}

successors! {
    pub(super) enum PauseNext<T> {
        Elapsed(Step<T>),
        Superseded(Choose),
        PoolLost(Choose),
        Unreadable(Recover),
    }
}

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Pause<T>
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = PauseNext<T>;

    /// A new target for the running tuner does not end the wait; it
    /// reaches the tuner at the next step. No temperature reading left to
    /// watch the machine by leads to [`Recover`], because a halt that
    /// waited for a reading could never end: the halt stops the readings.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> PauseNext<T> {
        let Self {
            mut run,
            until,
            unreadable_grace,
        } = self;
        let thermal = context.miner.thermal;
        match select4(
            Timer::at(until),
            run.selected.superseded(context.config),
            run.pool.lost(context.miner.pool_serial()),
            run.temperature.unreadable(thermal, unreadable_grace),
        )
        .await
        {
            Either4::First(()) => PauseNext::Elapsed(Step { run }),
            Either4::Second(()) => PauseNext::Superseded(Choose),
            Either4::Third(()) => {
                warn!("mining: no pool has held a job for the grace period; cutting rail");
                crate::events::pool_lost();
                PauseNext::PoolLost(Choose)
            }
            Either4::Fourth(()) => {
                error!("mining: temperature unreadable; rail cut");
                crate::events::temperature_unreadable();
                PauseNext::Unreadable(Recover::unreadable())
            }
        }
    }
}

/// Hold the rail off before the next bring-up.
pub(super) struct Recover {
    /// Whether the rail is off because no temperature reading was left,
    /// which the API reports as the reason of the stop.
    pub(super) unreadable: bool,
}

impl Recover {
    /// The state after a bring-up or a plan failed.
    pub(super) const fn after_fault() -> Self {
        Self { unreadable: false }
    }

    /// The state after no temperature reading was left.
    pub(super) const fn unreadable() -> Self {
        Self { unreadable: true }
    }
}

successors! { pub(super) enum RecoverNext { Retry(Choose) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Recover
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = RecoverNext;

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

    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> NoTunerNext {
        self.selected.superseded(context.config).await;
        NoTunerNext::Configured(Choose)
    }
}

/// Hold the rail off until nothing holds the machine: a resume, after a
/// halt from the manager or after a park, and a clear dangerous temperature
/// flag.
pub(super) struct Halted {
    /// Whether the manager still owes a resume. The flag of thermal is read
    /// where it is kept, so the manager cannot resume a machine it holds.
    awaits_resume: bool,
}

impl Halted {
    /// The state after a halt from the manager or after a park.
    pub(super) const fn until_resume() -> Self {
        Self {
            awaits_resume: true,
        }
    }

    /// The state after the dangerous temperature flag was set, with no
    /// halt from the manager.
    pub(super) const fn while_dangerous() -> Self {
        Self {
            awaits_resume: false,
        }
    }
}

successors! { pub(super) enum HaltedNext { Resume(Choose) } }

impl<R, T, const CHAINS: usize> State<Context<'_, R, T, CHAINS>> for Halted
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    type Next = HaltedNext;

    /// A resumed machine has been asked for the whole budget of empty
    /// bring-up cycles again, so the count starts from zero.
    async fn run(self, context: &mut Context<'_, R, T, CHAINS>) -> HaltedNext {
        info!("mining: rail down; awaiting resume");
        let thermal = context.miner.thermal;
        let mut awaits_resume = self.awaits_resume;
        while awaits_resume || thermal.is_dangerous() {
            if awaits_resume {
                awaits_resume = matches!(context.miner.mining.next_message().await, Message::Halt);
            } else if let Either::First(message) =
                select(context.miner.mining.next_message(), danger_cleared(thermal)).await
            {
                awaits_resume = matches!(message, Message::Halt);
            }
        }
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
    pub(super) failures: u8,
    pub(super) pool: PoolWatch,
    /// How long thermal has had no reading to publish.
    pub(super) temperature: TemperatureWatch,
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

    fn serves(self, tuning: &Tuning) -> bool {
        self == Self::of(tuning)
    }

    async fn superseded(self, subscription: &mut ConfigSubscription) {
        while self.serves(&subscription.changed().await.miner.tuning) {}
    }
}
