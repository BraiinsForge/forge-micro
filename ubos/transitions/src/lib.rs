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
//! A transition that no state chooses, because it interrupts whatever a
//! state is doing, is the machine's [`Machine::Anywhere`]: a `Next`
//! that applies to every state.
//!
//! The machine's owner holds the sum type over all states and the loop
//! that runs one state after another, or races a state against its
//! interrupts. That keeps the crate free of any executor.

#![no_std]

mod check;
mod edge;
mod machine;

pub use crate::check::{Fault, check};
pub use crate::edge::{Edge, Edges};
pub use crate::machine::{Lift, Machine, Member, Members, Named, State, member};
