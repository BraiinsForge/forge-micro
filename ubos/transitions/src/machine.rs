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

//! The traits a machine implements: its states, the sum type over them,
//! and the graph of their declared edges that the check reads.

use core::future::Future;

use crate::edge::{Edge, Edges};

#[cfg(test)]
#[path = "tests/machine.rs"]
mod tests;

/// The name of a state in reports of its edges.
pub trait Named {
    const NAME: &'static str;
}

/// Turns a state's `Next` into the machine's sum type of states.
pub trait Lift<Any> {
    fn lift(self) -> Any;
}

/// One state of machine `M`.
pub trait State<M: Machine>: Named + Sized {
    /// Every state this one may lead to.
    type Next: Edges + Lift<M::Any>;

    /// Do the state's work against the machine and return the state it
    /// leads to.
    fn run(self, machine: &mut M) -> impl Future<Output = Self::Next>;
}

/// A state of the machine in its graph: its name and its declared
/// edges.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub name: &'static str,
    pub edges: &'static [Edge],
}

/// The [`Node`] of state `S` in machine `M`, for its [`Graph`].
#[must_use]
pub const fn node<M: Machine, S: State<M>>() -> Node {
    Node {
        name: S::NAME,
        edges: <S::Next as Edges>::EDGES,
    }
}

/// The graph of machine `M`: a node for every state, listed by its
/// sum type.
pub trait Graph<M: Machine> {
    const NODES: &'static [Node];
}

/// A state machine: the sum type over its states and the transitions
/// that apply to every state. The states run against the machine, so it
/// is also what they borrow to do their work.
pub trait Machine: Sized {
    type Any: Graph<Self>;
    /// A machine without such a transition names an empty enum.
    type Anywhere: Edges + Lift<Self::Any>;
}
