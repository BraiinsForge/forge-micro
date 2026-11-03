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

//! State machine types: a state is a struct that owns its data, and the
//! states it may lead to are the type it returns.
//!
//! A [`State`] runs and returns its [`State::Next`], an enum with one
//! variant per edge, declared by [`successors!`]. A state cannot return
//! a state its `Next` does not name, so the code cannot take an
//! undeclared transition. The edges a machine declares are also data:
//! [`check`] walks them and reports a state no path reaches or a state
//! whose own edges never lead back to the start.
//!
//! [`machine!`] declares the sum type over the states, and the list in it
//! is the only place that names every state. The sum type is the
//! machine's [`Graph`] and its [`Machine`], whose `run` does the work of
//! the current state and returns the next one.
//!
//! A transition that no state chooses, because it interrupts whatever a
//! state is doing, is the machine's [`Graph::Anywhere`]: a `Next` that
//! applies to every state.
//!
//! The states run against a context, which is any type the states borrow
//! to do their work. The machine's owner holds the context and the loop
//! that runs one state after another, or races a state against its
//! interrupts. That keeps the crate free of any executor.
//!
//! # Example
//!
//! The machine has three states. `Idle` leads to `Work`, `Work` repeats
//! until its count runs out and then leads to `Done`, and `Done` leads
//! back to `Idle`. `Done` is also where the interrupt leads from every
//! state.
//!
//! ```
//! use core::future::Future;
//! use core::pin::pin;
//! use core::task::{Context, Poll, Waker};
//! use transitions::{Machine, State, check, machine, successors};
//!
//! // Each state is a struct that owns its data.
//! struct Idle;
//! struct Work {
//!     left: u32,
//! }
//! struct Done;
//!
//! // Each enum names the states one state may lead to. The variant is
//! // the trigger and its payload is the state it leads to.
//! successors! { enum IdleNext { Start(Work) } }
//! successors! { enum WorkNext { Again(Work), Finish(Done) } }
//! successors! { enum DoneNext { Reset(Idle) } }
//! // The interrupt applies to every state.
//! successors! { enum Anywhere { Stop(Done) } }
//!
//! // The sum type holds the state the machine is in. The name of a state
//! // in a report of a bad edge is its variant.
//! machine! {
//!     enum MachineState {
//!         Idle(Idle),
//!         Work(Work),
//!         Done(Done),
//!     }
//!     anywhere = Anywhere;
//! }
//!
//! // The context is what the states borrow to do their work.
//! struct Counter {
//!     steps: u32,
//! }
//!
//! // A state does its work and returns the enum of its edges, so a
//! // return that the enum does not name does not compile.
//! impl State<Counter> for Idle {
//!     type Next = IdleNext;
//!     async fn run(self, _counter: &mut Counter) -> IdleNext {
//!         IdleNext::Start(Work { left: 3 })
//!     }
//! }
//!
//! impl State<Counter> for Work {
//!     type Next = WorkNext;
//!     async fn run(self, counter: &mut Counter) -> WorkNext {
//!         counter.steps += 1;
//!         match self.left {
//!             0 | 1 => WorkNext::Finish(Done),
//!             left => WorkNext::Again(Work { left: left - 1 }),
//!         }
//!     }
//! }
//!
//! impl State<Counter> for Done {
//!     type Next = DoneNext;
//!     async fn run(self, _counter: &mut Counter) -> DoneNext {
//!         DoneNext::Reset(Idle)
//!     }
//! }
//!
//! // These states never wait, so one poll finishes a step. A machine
//! // whose states wait for hardware runs the same calls on an executor.
//! fn finish<F: Future>(future: F) -> F::Output {
//!     let mut future = pin!(future);
//!     match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
//!         Poll::Ready(output) => output,
//!         Poll::Pending => panic!("these states never wait"),
//!     }
//! }
//!
//! // The declared edges are sound: every state is reachable from `Idle`
//! // and has a path of its own edges back to it.
//! assert_eq!(check::<MachineState, Counter>("Idle"), Ok(()));
//!
//! let mut counter = Counter { steps: 0 };
//! let mut state = MachineState::Idle(Idle);
//! while !matches!(state, MachineState::Done(_)) {
//!     state = finish(state.run(&mut counter));
//! }
//! assert_eq!(counter.steps, 3);
//! ```
//!
//! # Checking the edges
//!
//! [`check`] holds the declared edges to two rules. Every state is
//! reachable from the start, so no state is one that no code can enter.
//! Every state has a path of its own edges back to the start, so no
//! state is one that only an interrupt can leave. The machine's
//! `Anywhere` edges count when a state is reached and do not count when
//! one is left, because an interrupt that applies to every state would
//! otherwise let every state pass. A failed rule is a [`Fault`] that
//! names the state, and a machine's own tests assert the result on every
//! build.

#![no_std]

mod check;
mod edge;
#[cfg(test)]
#[path = "tests/example.rs"]
mod example;
mod machine;

pub use crate::check::{Fault, check};
pub use crate::edge::{Edge, Edges};
pub use crate::machine::{Graph, Lift, Machine, Named, Node, State, node};
