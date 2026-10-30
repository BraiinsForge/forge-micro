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

use core::task::Poll;

use crate::example::{Any, Example, Idle, Work, poll_once};

#[test]
fn a_run_lifts_its_next_into_the_sum_type() {
    let mut machine = Example { idle_runs: 0 };
    let Poll::Ready(next) = poll_once(Any::Idle(Idle).run(&mut machine)) else {
        panic!("Idle::run must finish without waiting");
    };
    assert_eq!(next.name(), "Work");
    assert_eq!(machine.idle_runs, 1);
}

#[test]
fn a_state_that_waits_stays_in_its_state() {
    let mut machine = Example { idle_runs: 0 };
    assert!(poll_once(Any::Work(Work).run(&mut machine)).is_pending());
}
