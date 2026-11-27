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

//! The state machine of the miner: the sum type over its states, the
//! context they run against, and the one select that runs them.

use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;

use embassy_futures::select::{Either, Either3, select};
use embassy_time::{Duration, with_timeout};
use transitions::{Lift, Machine, machine, successors};
use ubos_config::ConfigSubscription;
use ubos_logging::{info, warn};

use super::Miner;
use super::states::{
    BringUp, Choose, Enable, ExitReset, Halted, NoTuner, Pause, Recover, Reset, StartWork, Step,
    WaitForPool,
};
use super::temperature::danger_raised;
use crate::control::MiningState;
use crate::reset::ResetPin;
use crate::tuner::MachineTuner;

/// How long an interrupted hardware plan has to finish before the miner
/// drops it and asks for the rail cut. The PSU task then gives a call
/// still on the bus its own `COMMAND_GRACE` before it starts the cut.
pub(super) const INTERRUPT_TIMEOUT: Duration = Duration::from_millis(100);

impl<R, T, const CHAINS: usize> Miner<R, T, CHAINS>
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    /// Drive the mining state machine from [`Choose`] until
    /// [`Handle::stop`](crate::Handle::stop) fires, which on bare metal is
    /// never. The cut before the first state means each board begins
    /// bring-up from a known-off supply, whatever the previous boot left
    /// it in.
    pub async fn run(&mut self, config: ubos_config::Handle) {
        let mut subscription = config.subscribe();
        // This task is spawned before the configuration task, and the
        // first state reads the tuning through this subscription, so it
        // waits for the first published config.
        subscription.changed().await;
        self.cut_rail().await;
        let mut state = MinerState::Choose(Choose);
        self.mining.publish(state.mining_state());
        while let Some(next) = self.advance(state, &mut subscription).await {
            state = next;
        }
    }

    /// Run `state` to its transition and return the next state, or `None`
    /// when [`Handle::stop`](crate::Handle::stop) ends the machine. The
    /// race runs first and the cut and the status publication follow it,
    /// so nothing cancels the cut halfway.
    pub(super) async fn advance(
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
    /// `None` when STOP wins. A HALT is a message from the manager or the
    /// dangerous temperature flag.
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
        let stop = self.mining.stopped();
        let halt = reads_halt.then(|| self.mining.halted());
        let thermal = self.thermal;
        let mut context = Context {
            miner: self,
            config,
        };
        let mut work = pin!(state.run(&mut context));
        let mut stop = pin!(stop);
        let mut halt = pin!(async {
            if let Some(from_manager) = halt {
                next_halt(from_manager, thermal).await
            } else {
                core::future::pending().await
            }
        });
        // Whether the work has waited on something, which a state that
        // writes to the hardware does from its first command on.
        let mut waited = false;
        let outcome = poll_fn(|cx| {
            if let Poll::Ready(()) = stop.as_mut().poll(cx) {
                return Poll::Ready(Either3::First(()));
            }
            if let Poll::Ready(halted) = halt.as_mut().poll(cx) {
                return Poll::Ready(Either3::Second(halted));
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
            Either3::Second(halted) => {
                warn!("mining: halt asserted");
                (Some(Anywhere::Halt(halted).lift()), true)
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
}

/// Wait for what halts a machine that is not halted, and return the state
/// it leads to. Only a halt from the manager needs a resume from the
/// manager.
async fn next_halt(
    from_manager: impl Future<Output = ()>,
    thermal: ubos_thermal::Handle,
) -> Halted {
    match select(from_manager, danger_raised(thermal)).await {
        Either::First(()) => Halted::until_resume(),
        Either::Second(()) => Halted::while_dangerous(),
    }
}

/// What a state runs against: the miner, and the configuration
/// subscription the state reads the tuning through. The subscription is
/// a borrow, so a test drives the machine from a configuration of its
/// own.
pub(super) struct Context<'m, R, T, const CHAINS: usize>
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    pub(super) miner: &'m mut Miner<R, T, CHAINS>,
    pub(super) config: &'m mut ConfigSubscription,
}

machine! {
    /// The state the miner is in.
    ///
    /// Each state says what it does with the rail in [`MinerState::rail`].
    pub(super) enum MinerState<T> {
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
    pub(super) enum Anywhere { Halt(Halted) }
}

/// What a state does with the rail.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Rail {
    /// The rail is up or about to be raised, so leaving for a state that
    /// holds it off cuts it. [`Reset`] and [`WaitForPool`] are this with
    /// the rail still off, so a halt or a stop during the pool wait cuts
    /// it again.
    Live,
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
    pub(super) fn rail(&self) -> Rail {
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
    pub(super) fn reads_halt(&self) -> bool {
        !matches!(self, Self::Halted(_))
    }

    /// Whether a STOP or HALT lets this state finish the command it waits
    /// on before the cut. Only [`Step`] writes plans to the hardware.
    pub(super) fn grants_grace(&self) -> bool {
        matches!(self, Self::Step(_))
    }

    /// The lifecycle status the API and the metrics report for this state.
    pub(super) fn mining_state(&self) -> MiningState {
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
