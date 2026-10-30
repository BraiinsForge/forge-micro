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

//! The machine the tests share: four states, the edges between them, and
//! an interrupt that leads every state to `Halt`.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use crate::{Machine, Member, Members, Named, State, member, successors};

pub(crate) struct Idle;
pub(crate) struct Start;
pub(crate) struct Work;
pub(crate) struct Halt;

/// Every state of the example machine.
pub(crate) enum Any {
    Idle(Idle),
    Start(Start),
    Work(Work),
    Halt(Halt),
}

macro_rules! state {
    ($state:ident) => {
        impl Named for $state {
            const NAME: &'static str = stringify!($state);
        }

        impl From<$state> for Any {
            fn from(state: $state) -> Self {
                Self::$state(state)
            }
        }
    };
}

state!(Idle);
state!(Start);
state!(Work);
state!(Halt);

successors! { pub(crate) enum IdleNext { Prepare(Start), Go(Work) } }
successors! { pub(crate) enum StartNext { StartSendingWork(Work) } }
successors! { #[allow(dead_code)] pub(crate) enum WorkNext { Finish(Halt) } }
successors! { pub(crate) enum Nowhere {} }
successors! { pub(crate) enum HaltNext { Reset(Idle) } }
successors! { pub(crate) enum Anywhere { Halt(Halt) } }

/// The example machine counts the runs of its idle state.
pub(crate) struct Example {
    pub(crate) idle_runs: u32,
}

impl Machine for Example {
    type Any = Any;
    type Anywhere = Anywhere;
}

impl Members<Example> for Any {
    const ALL: &'static [Member] = &[
        member::<Example, Idle>(),
        member::<Example, Start>(),
        member::<Example, Work>(),
        member::<Example, Halt>(),
    ];
}

impl Any {
    pub(crate) async fn run(self, machine: &mut Example) -> Self {
        match self {
            Self::Idle(state) => crate::Lift::lift(state.run(machine).await),
            Self::Start(state) => crate::Lift::lift(state.run(machine).await),
            Self::Work(state) => crate::Lift::lift(state.run(machine).await),
            Self::Halt(state) => crate::Lift::lift(state.run(machine).await),
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Idle(_) => Idle::NAME,
            Self::Start(_) => Start::NAME,
            Self::Work(_) => Work::NAME,
            Self::Halt(_) => Halt::NAME,
        }
    }
}

impl State<Example> for Idle {
    type Next = IdleNext;
    async fn run(self, machine: &mut Example) -> IdleNext {
        machine.idle_runs += 1;
        IdleNext::Go(Work)
    }
}

impl State<Example> for Start {
    type Next = StartNext;
    async fn run(self, _machine: &mut Example) -> StartNext {
        StartNext::StartSendingWork(Work)
    }
}

impl State<Example> for Work {
    type Next = WorkNext;
    async fn run(self, _machine: &mut Example) -> WorkNext {
        core::future::pending().await
    }
}

impl State<Example> for Halt {
    type Next = HaltNext;
    async fn run(self, _machine: &mut Example) -> HaltNext {
        HaltNext::Reset(Idle)
    }
}

/// Run `future` to its first suspension.
pub(crate) fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut future = pin!(future);
    future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
}
