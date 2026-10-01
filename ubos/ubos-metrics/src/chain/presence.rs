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

//! A chain's enumeration outcome and its packed storage form. The
//! decision table that produces one is the miner's, in
//! `ubos_mining::chain::presence`.

#[cfg(test)]
#[path = "tests/presence.rs"]
mod tests;

/// A chain's enumeration outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainPresence {
    Unknown,
    Present {
        chips: u8,
    },
    /// Zero chips answered: no hashboard is attached.
    Absent,
    /// A wrong chip count, or the right count and then a refused
    /// safe-boot configuration, on every attempt. `found` is the last
    /// attempt's count, so it equals the expected count after a refused
    /// configuration.
    Faulted {
        found: u8,
    },
}

/// Storage codes for [`ChainPresence`]; [`ChainPresence::payload`] is
/// stored beside them.
const CODE_UNKNOWN: u8 = 0;
const CODE_PRESENT: u8 = 1;
const CODE_ABSENT: u8 = 2;
const CODE_FAULTED: u8 = 3;

impl ChainPresence {
    #[must_use]
    pub fn is_present(self) -> bool {
        matches!(self, Self::Present { .. })
    }

    /// Zero for any state but `Present`.
    #[must_use]
    pub fn chips(self) -> u8 {
        match self {
            Self::Present { chips } => chips,
            Self::Unknown | Self::Absent | Self::Faulted { .. } => 0,
        }
    }

    /// The payload stored beside [`Self::as_code`]. Unlike [`Self::chips`]
    /// it keeps a `Faulted` count, so a stored fault still says what was
    /// found.
    #[must_use]
    pub(crate) fn payload(self) -> u8 {
        match self {
            Self::Present { chips } => chips,
            Self::Faulted { found } => found,
            Self::Unknown | Self::Absent => 0,
        }
    }

    #[must_use]
    pub(crate) fn as_code(self) -> u8 {
        match self {
            Self::Unknown => CODE_UNKNOWN,
            Self::Present { .. } => CODE_PRESENT,
            Self::Absent => CODE_ABSENT,
            Self::Faulted { .. } => CODE_FAULTED,
        }
    }

    /// An unknown code reads as `Unknown`, so a store read before its
    /// first write never panics.
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
