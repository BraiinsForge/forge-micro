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

//! Bitcoin pool difficulty derived from the 256-bit share target.
//!
//! The formal definition is `difficulty = (0xffff << 208) / target`.
//! For defmt-friendly display we approximate it with the second
//! 32-bit word (MSB-first) of the target:
//!
//! ```text
//! diff ≈ 0xffff_0000 / target_be[4..8]
//! ```
//!
//! Approximation cost: one u32 division, no 256-bit math, sentinel
//! values when the result would not fit in `u32`. Range covers
//! every diff a mining-pool worker realistically sees (diff 1 up to
//! several billion).

use core::fmt::Write as _;

use heapless::String;

#[cfg(test)]
#[path = "tests/difficulty.rs"]
mod tests;

/// Maximum byte length of a [`Target::approx_string`] result. Sized for
/// `"4294901760"` (10 chars) plus the longest sentinel `">MAX"`.
pub const APPROX_STRING_CAP: usize = 12;

/// A 256-bit share target in little-endian byte order, so the byte at
/// index 0 is the least significant. It gives the pool difficulty as a
/// display figure and as a number, from one approximation, so a metric
/// shows the same value as the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target([u8; 32]);

/// Result of the approximate-difficulty calculation. `BelowOne` and
/// `OverflowMax` are the two sentinel cases the u32 approximation
/// cannot represent numerically; [`Target::approx_string`] turns them
/// into readable strings, and [`Target::difficulty`] into `None`.
enum ApproxDifficulty {
    BelowOne,
    OverflowMax,
    Value(u32),
}

impl Target {
    /// Wrap the target bytes, least significant byte first.
    #[must_use]
    pub const fn from_le_array(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Classify the target into a rough difficulty.
    ///
    /// Reads the top four bytes only, which is enough for a display
    /// figure and avoids 256-bit division. A target whose top word is
    /// larger than difficulty 1 classifies as
    /// [`ApproxDifficulty::BelowOne`], and one small enough to overflow
    /// `u32` classifies as [`ApproxDifficulty::OverflowMax`]. Both
    /// callers render those two cases rather than printing a wrong
    /// number.
    fn approx_difficulty(&self) -> ApproxDifficulty {
        let top4 = u32::from_le_bytes(
            self.0[28..32]
                .try_into()
                .expect("BUG: 4-byte slice from a 32-byte array"),
        );
        if top4 != 0 {
            return ApproxDifficulty::BelowOne;
        }
        let next4 = u32::from_le_bytes(
            self.0[24..28]
                .try_into()
                .expect("BUG: 4-byte slice from a 32-byte array"),
        );
        if next4 == 0 {
            return ApproxDifficulty::OverflowMax;
        }
        ApproxDifficulty::Value(0xffff_0000_u32 / next4)
    }

    /// Format the approximate difficulty as a defmt-friendly string.
    ///
    /// Output:
    ///
    /// - `"<1"` if the target's top 32 MSB-first bits are nonzero
    ///   (difficulty below 1; usually a misconfigured local target).
    /// - `">MAX"` if the next 32 MSB-first bits are zero (difficulty
    ///   so high the u32 approximation overflows; pool routinely
    ///   produces this only at very large worker hashrates).
    /// - the decimal `0xffff_0000 / target_be[4..8]` otherwise.
    ///
    /// # Panics
    ///
    /// Never — the `String` constructors below always fit within
    /// [`APPROX_STRING_CAP`]. The `expect` calls document the
    /// invariant for the reader.
    #[must_use]
    pub fn approx_string(&self) -> String<APPROX_STRING_CAP> {
        match self.approx_difficulty() {
            ApproxDifficulty::BelowOne => {
                String::try_from("<1").expect("fits in APPROX_STRING_CAP")
            }
            ApproxDifficulty::OverflowMax => {
                String::try_from(">MAX").expect("fits in APPROX_STRING_CAP")
            }
            ApproxDifficulty::Value(diff) => {
                let mut out: String<APPROX_STRING_CAP> = String::new();
                let _ = write!(out, "{diff}");
                out
            }
        }
    }

    /// Give the approximate difficulty as a number, in the diff-1-share
    /// units that a sum of work counts in. It is `None` below difficulty 1
    /// and past the u32 approximation, because neither case has a number
    /// that is true, and a caller that sums work counts it as no work.
    #[must_use]
    pub fn difficulty(&self) -> Option<u64> {
        match self.approx_difficulty() {
            ApproxDifficulty::BelowOne | ApproxDifficulty::OverflowMax => None,
            ApproxDifficulty::Value(diff) => Some(u64::from(diff)),
        }
    }
}
