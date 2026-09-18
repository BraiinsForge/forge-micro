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

//! A chain's enumeration outcome, as the per-chain table stores it.
//!
//! The miner decides a chain's presence and writes it into that
//! chain's [`super::ChainStats`]; the API, the dashboard, the thermal
//! loop and the metrics publisher read it back from there. The decision
//! table that produces a presence from an enumeration is the miner's
//! own, in `ubos_mining::chain::presence`; this module holds only the
//! value and its packed storage form.

#[cfg(test)]
#[path = "tests/presence.rs"]
mod tests;

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
    /// The chain answered with a chip count other than expected, or
    /// answered the expected count and then refused its safe-boot
    /// configuration, on every attempt up to the retry budget. `found`
    /// is the count of the last attempt, so it equals the expected count
    /// for a chain that failed configuration.
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
    pub(crate) fn payload(self) -> u8 {
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
    pub(crate) fn as_code(self) -> u8 {
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
    pub(crate) fn from_code(code: u8, payload: u8) -> Self {
        match code {
            CODE_PRESENT => Self::Present { chips: payload },
            CODE_ABSENT => Self::Absent,
            CODE_FAULTED => Self::Faulted { found: payload },
            _ => Self::Unknown,
        }
    }
}
