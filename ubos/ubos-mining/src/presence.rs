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

/// Reset and enumerate attempts a chain gets before its answer is
/// treated as final.
pub const ENUMERATE_ATTEMPTS: u8 = 3;

/// A chain's enumeration outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainPresence {
    /// No enumeration attempt has completed yet.
    Unknown,
    /// The chain answered with the expected chip count.
    Present { chips: u8 },
    /// The connector answered with zero chips: no hashboard is
    /// attached.
    Absent,
    /// The chain answered with a chip count other than expected, on
    /// every attempt up to the retry budget.
    Faulted { found: u8 },
}

/// Discriminant codes for [`ChainPresence`], used to store the state in
/// a `ChainStats` atomic. `found`/`chips` are not part of the code; the
/// caller stores [`ChainPresence::payload`] alongside it.
const CODE_UNKNOWN: u8 = 0;
const CODE_PRESENT: u8 = 1;
const CODE_ABSENT: u8 = 2;
const CODE_FAULTED: u8 = 3;

impl ChainPresence {
    /// True for a chain the miner should ramp and mine on.
    #[must_use]
    pub fn is_present(self) -> bool {
        matches!(self, Self::Present { .. })
    }

    /// The chip count this chain answered with, or zero for any state
    /// that is not `Present`.
    #[must_use]
    pub fn chips(self) -> u8 {
        match self {
            Self::Present { chips } => chips,
            Self::Unknown | Self::Absent | Self::Faulted { .. } => 0,
        }
    }

    /// The per-variant payload to store next to [`Self::as_code`]: the
    /// chip count of a `Present` chain, the mismatched count of a
    /// `Faulted` one, zero otherwise. Unlike [`Self::chips`] it keeps
    /// the `Faulted` count, so a stored fault still says what was found.
    #[must_use]
    pub fn payload(self) -> u8 {
        match self {
            Self::Present { chips } => chips,
            Self::Faulted { found } => found,
            Self::Unknown | Self::Absent => 0,
        }
    }

    /// Discriminant code for storage in a `ChainStats` atomic. Carries
    /// no `found`/`chips` payload; the caller stores [`Self::payload`]
    /// separately and reattaches it in [`Self::from_code`].
    #[must_use]
    pub fn as_code(self) -> u8 {
        match self {
            Self::Unknown => CODE_UNKNOWN,
            Self::Present { .. } => CODE_PRESENT,
            Self::Absent => CODE_ABSENT,
            Self::Faulted { .. } => CODE_FAULTED,
        }
    }

    /// Reconstruct a `ChainPresence` from a stored code and the
    /// [`Self::payload`] the same store keeps alongside it. The payload
    /// becomes `chips` for `Present`, `found` for `Faulted`, and is
    /// ignored otherwise; an unrecognized code falls back to `Unknown`
    /// so a store read before its first write never panics.
    #[must_use]
    pub fn from_code(code: u8, payload: u8) -> Self {
        match code {
            CODE_PRESENT => Self::Present { chips: payload },
            CODE_ABSENT => Self::Absent,
            CODE_FAULTED => Self::Faulted { found: payload },
            _ => Self::Unknown,
        }
    }
}

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
/// A connector that answers zero chips has no hashboard behind it, so
/// it is parked as `Absent` on the first attempt with no retry:
/// retrying an empty connector three times on every bring-up would cost
/// the machine its bring-up time for nothing. A non-zero count that
/// does not match `expected` is retried up to [`ENUMERATE_ATTEMPTS`]
/// before it is parked as `Faulted`.
#[must_use]
pub fn transition(found: u8, expected: u8, attempt: u8) -> (ChainPresence, Action) {
    if found == expected {
        return (ChainPresence::Present { chips: found }, Action::Continue);
    }
    if found == 0 {
        return (ChainPresence::Absent, Action::Park);
    }
    if attempt < ENUMERATE_ATTEMPTS {
        (ChainPresence::Unknown, Action::Retry)
    } else {
        (ChainPresence::Faulted { found }, Action::Park)
    }
}
