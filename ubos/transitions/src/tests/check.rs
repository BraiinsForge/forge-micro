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

use crate::example::{Anywhere, Example, Halt, Idle, MachineState, Nowhere, Work};
use crate::{Fault, Graph, Named, Node, check, successors};

#[test]
fn a_sound_machine_passes() {
    assert_eq!(check::<MachineState, Example>("Idle"), Ok(()));
}

/// Graphs whose declared edges are unsound. They are written by hand,
/// with no states that run, so they are graphs over the empty context.
mod unsound {
    use super::*;
    use crate::Edges;

    successors! { #[allow(dead_code)] enum ToWork { Go(Work) } }
    successors! { #[allow(dead_code)] enum ToIdle { Back(Idle) } }

    /// Idle leads to Work and Work back to Idle; no edge leads to Halt.
    struct Orphan;

    impl Graph<()> for Orphan {
        const NODES: &'static [Node] = &[
            Node {
                name: Idle::NAME,
                edges: ToWork::EDGES,
            },
            Node {
                name: Work::NAME,
                edges: ToIdle::EDGES,
            },
            Node {
                name: Halt::NAME,
                edges: ToIdle::EDGES,
            },
        ];
        type Anywhere = Nowhere;
    }

    /// Idle leads to Work, and Work has no edge.
    struct Stuck;

    impl Graph<()> for Stuck {
        const NODES: &'static [Node] = &[
            Node {
                name: Idle::NAME,
                edges: ToWork::EDGES,
            },
            Node {
                name: Work::NAME,
                edges: Nowhere::EDGES,
            },
        ];
        type Anywhere = Nowhere;
    }

    /// Work has no edge of its own, and only the interrupt leaves it.
    struct Interrupted;

    impl Graph<()> for Interrupted {
        const NODES: &'static [Node] = &[
            Node {
                name: Idle::NAME,
                edges: ToWork::EDGES,
            },
            Node {
                name: Work::NAME,
                edges: Nowhere::EDGES,
            },
            Node {
                name: Halt::NAME,
                edges: ToIdle::EDGES,
            },
        ];
        type Anywhere = Anywhere;
    }

    #[test]
    fn a_state_only_an_interrupt_leaves_is_reported() {
        assert_eq!(
            check::<Interrupted, ()>("Idle"),
            Err(Fault::DeadEnd("Work"))
        );
    }

    #[test]
    fn a_state_without_an_inbound_edge_is_reported() {
        assert_eq!(check::<Orphan, ()>("Idle"), Err(Fault::Unreachable("Halt")));
    }

    #[test]
    fn a_state_with_no_way_out_is_reported() {
        assert_eq!(check::<Stuck, ()>("Idle"), Err(Fault::DeadEnd("Work")));
    }

    #[test]
    fn an_unknown_start_is_reported() {
        assert_eq!(
            check::<Stuck, ()>("Start"),
            Err(Fault::UnknownState("Start"))
        );
    }
}
