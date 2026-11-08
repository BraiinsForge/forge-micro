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

//! Positions inside the machine: which hashboard a series or a reading
//! belongs to. Every crate that names a board by number shares this type,
//! so the metrics, temperature and mining crates agree on what an index
//! is without depending on the chip driver.

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
