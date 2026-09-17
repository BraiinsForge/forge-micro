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

//! Coverage for every `transition` outcome and the retry budget
//! boundary.

use super::{Action, ChainPresence, ENUMERATE_ATTEMPTS, configure_failed, transition};

const EXPECTED: u8 = 4;

#[test]
fn matching_count_is_present_and_continues() {
    let (presence, action) = transition(EXPECTED, EXPECTED, 1);
    assert_eq!(presence, ChainPresence::Present { chips: EXPECTED });
    assert_eq!(action, Action::Continue);
}

#[test]
fn zero_chips_is_absent_and_parks_without_retry() {
    let (presence, action) = transition(0, EXPECTED, 1);
    assert_eq!(presence, ChainPresence::Absent);
    assert_eq!(action, Action::Park);
}

#[test]
fn zero_chips_on_last_attempt_is_still_absent_not_faulted() {
    let (presence, action) = transition(0, EXPECTED, ENUMERATE_ATTEMPTS);
    assert_eq!(presence, ChainPresence::Absent);
    assert_eq!(action, Action::Park);
}

#[test]
fn mismatched_count_retries_below_the_attempt_budget() {
    for attempt in 1..ENUMERATE_ATTEMPTS {
        let (presence, action) = transition(EXPECTED + 1, EXPECTED, attempt);
        assert_eq!(presence, ChainPresence::Unknown, "attempt {attempt}");
        assert_eq!(action, Action::Retry, "attempt {attempt}");
    }
}

#[test]
fn mismatched_count_at_the_attempt_budget_is_faulted_and_parks() {
    let (presence, action) = transition(EXPECTED + 1, EXPECTED, ENUMERATE_ATTEMPTS);
    assert_eq!(
        presence,
        ChainPresence::Faulted {
            found: EXPECTED + 1
        }
    );
    assert_eq!(action, Action::Park);
}

#[test]
fn presence_helpers_agree_with_the_variant() {
    assert!(ChainPresence::Present { chips: 4 }.is_present());
    assert!(!ChainPresence::Unknown.is_present());
    assert!(!ChainPresence::Absent.is_present());
    assert!(!ChainPresence::Faulted { found: 2 }.is_present());

    assert_eq!(ChainPresence::Present { chips: 4 }.chips(), 4);
    assert_eq!(ChainPresence::Unknown.chips(), 0);
    assert_eq!(ChainPresence::Absent.chips(), 0);
    assert_eq!(ChainPresence::Faulted { found: 2 }.chips(), 0);
}

#[test]
fn code_round_trips_through_present_and_faulted() {
    let present = ChainPresence::Present { chips: 7 };
    assert_eq!(
        ChainPresence::from_code(present.as_code(), present.payload()),
        present
    );

    let faulted = ChainPresence::Faulted { found: 3 };
    assert_eq!(faulted.payload(), 3);
    assert_eq!(
        ChainPresence::from_code(faulted.as_code(), faulted.payload()),
        faulted
    );

    assert_eq!(
        ChainPresence::from_code(ChainPresence::Absent.as_code(), 0),
        ChainPresence::Absent
    );
    assert_eq!(
        ChainPresence::from_code(ChainPresence::Unknown.as_code(), 0),
        ChainPresence::Unknown
    );
}

#[test]
fn configure_failure_retries_below_the_attempt_budget() {
    let (presence, action) = configure_failed(EXPECTED, ENUMERATE_ATTEMPTS - 1);
    assert_eq!(presence, ChainPresence::Unknown);
    assert_eq!(action, Action::Retry);
}

#[test]
fn configure_failure_at_the_attempt_budget_is_faulted_with_the_full_count() {
    let (presence, action) = configure_failed(EXPECTED, ENUMERATE_ATTEMPTS);
    assert_eq!(presence, ChainPresence::Faulted { found: EXPECTED });
    assert_eq!(action, Action::Park);
}

/// A descriptor claiming no chips cannot be met by any answer, so the
/// empty connector is read as absent rather than as a chain that
/// reached its target.
#[test]
fn a_zero_chip_descriptor_reads_an_empty_connector_as_absent() {
    let (presence, action) = transition(0, 0, 1);
    assert_eq!(presence, ChainPresence::Absent);
    assert_eq!(action, Action::Park);
}
