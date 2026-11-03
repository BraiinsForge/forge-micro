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

use crate::example::{Anywhere, Halt, IdleNext, MachineState, Nowhere, Start};

#[test]
fn the_edges_are_the_declared_ones() {
    let edges = <IdleNext as crate::Edges>::EDGES;
    let declared: [(&str, &str); 2] = [("Prepare", "Start"), ("Go", "Work")];
    assert_eq!(edges.len(), declared.len());
    for (edge, (trigger, to)) in edges.iter().zip(declared) {
        assert_eq!((edge.trigger, edge.to), (trigger, to));
    }
    assert!(<Nowhere as crate::Edges>::EDGES.is_empty());
}

#[test]
fn every_edge_lifts_to_the_state_it_names() {
    use crate::Lift;

    let to_start: MachineState = IdleNext::Prepare(Start).lift();
    let to_halt: MachineState = Anywhere::Halt(Halt).lift();
    assert!(matches!(to_start, MachineState::Start(_)));
    assert!(matches!(to_halt, MachineState::Halt(_)));
}
