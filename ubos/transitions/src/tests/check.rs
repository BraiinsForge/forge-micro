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

use crate::example::{Anywhere, Example, Halt, Idle, Nowhere, Work};
use crate::{Fault, Graph, Machine, Named, Node, check, successors};

#[test]
fn a_sound_machine_passes() {
    assert_eq!(check::<Example>("Idle"), Ok(()));
}

/// Machines whose declared edges are unsound. Only their edge lists are
/// read, so their variants are never built.
mod unsound {
    use super::*;
    use crate::Edges;

    successors! { #[allow(dead_code)] enum ToWork { Go(Work) } }
    successors! { #[allow(dead_code)] enum ToIdle { Back(Idle) } }
    /// Idle leads to Work and Work back to Idle; no edge leads to Halt.
    struct Orphan;

    impl Machine for Orphan {
        type Any = OrphanStates;
        type Anywhere = Nowhere;
    }

    struct OrphanStates;

    impl Graph<Orphan> for OrphanStates {
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
    }

    /// Idle leads to Work, and Work has no edge.
    struct Stuck;

    impl Machine for Stuck {
        type Any = StuckStates;
        type Anywhere = Nowhere;
    }

    struct StuckStates;

    impl Graph<Stuck> for StuckStates {
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
    }

    /// Work has no edge of its own, and only the interrupt leaves it.
    struct Interrupted;

    impl Machine for Interrupted {
        type Any = InterruptedStates;
        type Anywhere = Anywhere;
    }

    struct InterruptedStates;

    impl From<Halt> for InterruptedStates {
        fn from(_: Halt) -> Self {
            Self
        }
    }

    impl Graph<Interrupted> for InterruptedStates {
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
    }

    #[test]
    fn a_state_only_an_interrupt_leaves_is_reported() {
        assert_eq!(check::<Interrupted>("Idle"), Err(Fault::DeadEnd("Work")));
    }

    #[test]
    fn a_state_without_an_inbound_edge_is_reported() {
        assert_eq!(check::<Orphan>("Idle"), Err(Fault::Unreachable("Halt")));
    }

    #[test]
    fn a_state_with_no_way_out_is_reported() {
        assert_eq!(check::<Stuck>("Idle"), Err(Fault::DeadEnd("Work")));
    }

    #[test]
    fn an_unknown_start_is_reported() {
        assert_eq!(check::<Stuck>("Start"), Err(Fault::UnknownState("Start")));
    }
}
