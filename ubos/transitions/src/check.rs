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

//! The check that holds a machine's declared edges to the shape the
//! machine relies on.

use crate::edge::Edges;
use crate::machine::{Machine, Members};

#[cfg(test)]
#[path = "tests/check.rs"]
mod tests;

/// Why a machine's declared edges are unsound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The machine has more states than [`check`] can track.
    TooManyStates,
    /// An edge leads to a state the machine does not list.
    UnknownState(&'static str),
    /// No path from the start state reaches this state.
    Unreachable(&'static str),
    /// No path of this state's own edges reaches the start state, so only
    /// a [`Machine::Anywhere`] edge leads out of it.
    DeadEnd(&'static str),
}

/// The most states [`check`] tracks.
const MAX_STATES: usize = u64::BITS as usize;

/// Check the edges of machine `M` from its `start` state.
///
/// The check exists so a declared graph is held to the shape the machine
/// relies on: every state is reachable from `start`, and every state has
/// a path back to it, which keeps the machine from ending in a state it
/// cannot leave by its own work. The machine's [`Machine::Anywhere`]
/// edges count as edges of every state when reaching a state, and not
/// when leaving one, because an interrupt that applies to every state
/// would otherwise let every state pass.
///
/// # Errors
///
/// The first [`Fault`] found.
pub fn check<M: Machine>(start: &'static str) -> Result<(), Fault> {
    let members = <M::Any as Members<M>>::ALL;
    if members.len() > MAX_STATES {
        return Err(Fault::TooManyStates);
    }
    let position = |name: &'static str| {
        members
            .iter()
            .position(|member| member.name == name)
            .ok_or(Fault::UnknownState(name))
    };
    let start = position(start)?;

    // Bit `j` is set when there is an edge to state `j`: `anywhere` for
    // every state, and `next[i]` for state `i` alone.
    let mut anywhere = 0_u64;
    for edge in <M::Anywhere as Edges>::EDGES {
        anywhere |= 1 << position(edge.to)?;
    }
    let mut next = [0_u64; MAX_STATES];
    for (slot, member) in next.iter_mut().zip(members) {
        for edge in member.edges {
            *slot |= 1 << position(edge.to)?;
        }
    }

    let mut reached = 1_u64 << start;
    let mut leads_back = 1_u64 << start;
    loop {
        let (reached_before, leads_back_before) = (reached, leads_back);
        for (state, edges) in next.iter().enumerate().take(members.len()) {
            if (reached >> state) & 1 == 1 {
                reached |= edges | anywhere;
            }
            if edges & leads_back != 0 {
                leads_back |= 1 << state;
            }
        }
        if (reached, leads_back) == (reached_before, leads_back_before) {
            break;
        }
    }

    for (state, member) in members.iter().enumerate() {
        if (reached >> state) & 1 == 0 {
            return Err(Fault::Unreachable(member.name));
        }
        if (leads_back >> state) & 1 == 0 {
            return Err(Fault::DeadEnd(member.name));
        }
    }
    Ok(())
}
