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

//! The presence helpers and the packed code round trip.

use super::ChainPresence;

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
