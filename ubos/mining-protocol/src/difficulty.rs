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

/// Maximum byte length of an [`approx_string`] result. Sized for
/// `"4294901760"` (10 chars) plus the longest sentinel `">MAX"`.
pub const APPROX_STRING_CAP: usize = 12;

/// Result of the approximate-difficulty calculation. `BelowOne` and
/// `OverflowMax` are the two sentinel cases the u32 approximation
/// cannot represent numerically; [`approx_string`] turns them into
/// readable strings, [`approx_value`] into `0` / `u64::MAX`.
enum ApproxDifficulty {
    BelowOne,
    OverflowMax,
    Value(u32),
}

fn approx_difficulty(target_le: &[u8; 32]) -> ApproxDifficulty {
    let top4 = u32::from_le_bytes(
        target_le[28..32]
            .try_into()
            .expect("BUG: 4-byte slice from a 32-byte array"),
    );
    if top4 != 0 {
        return ApproxDifficulty::BelowOne;
    }
    let next4 = u32::from_le_bytes(
        target_le[24..28]
            .try_into()
            .expect("BUG: 4-byte slice from a 32-byte array"),
    );
    if next4 == 0 {
        return ApproxDifficulty::OverflowMax;
    }
    ApproxDifficulty::Value(0xffff_0000_u32 / next4)
}

/// Format the approximate difficulty of `target_le` as a defmt-
/// friendly string. `target_le` is the canonical 256-bit
/// little-endian share target (`target_le[0]` is the LSB).
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
/// Never — the internal `try_into` calls slice fixed offsets out
/// of a fixed-size array and the `String` constructors below
/// always fit within [`APPROX_STRING_CAP`]. The `expect` calls
/// document the invariants for the reader.
#[must_use]
pub fn approx_string(target_le: &[u8; 32]) -> String<APPROX_STRING_CAP> {
    match approx_difficulty(target_le) {
        ApproxDifficulty::BelowOne => String::try_from("<1").expect("fits in APPROX_STRING_CAP"),
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

/// Numeric counterpart of [`approx_string`] for code paths that
/// account for accepted work (e.g. V1 submit acceptance, where the
/// trait surface reports diff-1-share units to match SV2's
/// `SubmitSharesSuccess.new_shares_sum`).
///
/// Sentinel handling:
///
/// - `0` when the difficulty would be below 1.
/// - `u64::MAX` when the u32 approximation overflows.
#[must_use]
pub fn approx_value(target_le: &[u8; 32]) -> u64 {
    match approx_difficulty(target_le) {
        ApproxDifficulty::BelowOne => 0,
        ApproxDifficulty::OverflowMax => u64::MAX,
        ApproxDifficulty::Value(diff) => u64::from(diff),
    }
}
