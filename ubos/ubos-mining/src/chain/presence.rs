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

//! Chain presence: whether a hashboard connector answers enumeration,
//! and what the miner does about it.
//!
//! A machine can have more hashboard connectors than boards plugged
//! into them, so enumeration is a per-chain question rather than a
//! whole-machine one. [`transition`] is the pure decision table the
//! miner drives its enumeration loop with; it takes no I/O and holds
//! no state, so it is exhaustively testable without a chip.

#[cfg(test)]
#[path = "tests/presence.rs"]
mod tests;

use ubos_metrics::chain::ChainPresence;

/// Reset and enumerate attempts a chain gets before its answer is
/// treated as final.
pub const ENUMERATE_ATTEMPTS: u8 = 3;

/// What the miner does next after one reset-and-enumerate attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Keep bringing the machine up with this chain in its new state.
    Continue,
    /// Put this chain back in reset and leave it out of the ramp.
    Park,
    /// Reset and enumerate this chain again.
    Retry,
}

/// Decide a chain's presence and what to do next, given the chip count
/// `found` on this attempt, the board's `expected` chip count, and the
/// 1-based `attempt` number this call represents.
///
/// A chain passes only on the count its descriptor claims. Every other
/// answer is retried up to [`ENUMERATE_ATTEMPTS`], zero chips included,
/// because a receive error in the enumerate window leaves a populated
/// chain as mute as an empty connector and only the budget bounds the
/// mistake either way.
///
/// A parked chain is `Absent` when its last answer was nothing at all
/// and `Faulted` when it was a count that is not the expected one.
///
/// A zero `expected` is a descriptor that claims no chips, so the empty
/// answer reads as the absent connector it is and parks at once, no
/// retry being able to change it.
#[must_use]
pub fn transition(found: u8, expected: u8, attempt: u8) -> (ChainPresence, Action) {
    if expected == 0 {
        return (ChainPresence::Absent, Action::Park);
    }
    if found == expected {
        return (ChainPresence::Present { chips: found }, Action::Continue);
    }
    if attempt < ENUMERATE_ATTEMPTS {
        return (ChainPresence::Unknown, Action::Retry);
    }
    if found == 0 {
        return (ChainPresence::Absent, Action::Park);
    }
    (ChainPresence::Faulted { found }, Action::Park)
}

/// Decide what to do after a chain that enumerated `found` chips, the
/// expected count, refused its safe-boot configuration on the 1-based
/// `attempt`.
///
/// The chain gets the same [`ENUMERATE_ATTEMPTS`] budget a short count
/// does, because a refused baud switch or register write after a good
/// enumeration is the same class of transient glitch. On the last
/// attempt it is parked as `Faulted`, carrying the count it answered.
#[must_use]
pub fn configure_failed(found: u8, attempt: u8) -> (ChainPresence, Action) {
    if attempt < ENUMERATE_ATTEMPTS {
        (ChainPresence::Unknown, Action::Retry)
    } else {
        (ChainPresence::Faulted { found }, Action::Park)
    }
}
