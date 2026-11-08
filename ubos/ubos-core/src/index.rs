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

//! Positions inside the machine: which hashboard, which chip of a chain,
//! and which chips a command on the chain reaches. Every crate that names
//! a board or a chip by number shares these types, so the metrics,
//! temperature and mining crates agree on what an index is without
//! depending on the chip driver.

use core::fmt;

#[cfg(test)]
#[path = "tests/index.rs"]
mod tests;

/// Which hashboard of the machine, counted from zero. It is the index
/// that the metrics series and the temperature sources name a board by.
/// The number in `.0` is what goes into labels and events; [`Self::slot`]
/// is the position in a per-hashboard table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct HashboardIndex(pub u8);

impl HashboardIndex {
    /// The first `count` hashboards, in order. A count above the 256 that
    /// an index can name is cut to 256.
    pub fn range(count: usize) -> impl Iterator<Item = Self> {
        (0..=u8::MAX).take(count).map(Self)
    }

    /// The position of this hashboard in a table that holds one entry
    /// per hashboard.
    #[must_use]
    pub const fn slot(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for HashboardIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Which chips of a chain a command reaches: every chip, or the one at
/// the given index. A chip driver turns it into the address its protocol
/// puts on the wire. A chain of one chip is addressed to every chip, so
/// `All` stands for that lone chip wherever a single chip is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ChipAddress {
    /// Every chip of the chain.
    All,
    /// The chip at this index, counted from zero.
    One(u8),
}

impl ChipAddress {
    /// The first `count` chips of a chain, in order. A count above the 256
    /// that an index can name is cut to 256.
    pub fn range(count: usize) -> impl Iterator<Item = Self> {
        (0..=u8::MAX).take(count).map(Self::One)
    }

    /// Whether the address reaches every chip of the chain.
    #[must_use]
    pub const fn is_all(self) -> bool {
        matches!(self, Self::All)
    }

    /// The position of the chip in a table that holds one entry per
    /// chip. `All` is the lone chip of a chain of one, so it is position
    /// zero.
    #[must_use]
    pub const fn slot(self) -> usize {
        match self {
            Self::All => 0,
            Self::One(chip) => chip as usize,
        }
    }
}

impl fmt::Display for ChipAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => formatter.write_str("all"),
            Self::One(chip) => chip.fmt(formatter),
        }
    }
}
