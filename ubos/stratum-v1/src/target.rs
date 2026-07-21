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

//! Stratum V1 difficulty-to-target conversion.
//!
//! V1 advertises share difficulty as an `f64` over `mining.set_difficulty`.
//! The trait-side `SessionUpdate::SetTarget` carries 32 bytes in little-
//! endian 256-bit form; this module bridges the two.

#[cfg(test)]
#[path = "tests/target.rs"]
mod tests;

/// Convert a pool-supplied stratum difficulty to a 256-bit little-endian
/// target suitable for byte-by-byte hash comparison.
///
/// `target` = `truediffone` / `diff`, where `truediffone` is
/// `0xFFFF * 2^208`. `f64` precision is ~52 bits so the lowest bytes of
/// the target may be slightly off for exotic diffs, but the high bytes
/// that actually decide share acceptance are accurate.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn target_from_diff(diff: f64) -> [u8; 32] {
    if diff <= 0.0 {
        return [0xFFu8; 32];
    }
    // 2^208 as an exact IEEE-754 double (exponent 208 + bias 1023 = 1231).
    // Avoids relying on libm's powi in no_std builds.
    let pow2_208 = f64::from_bits(0x4CF0_0000_0000_0000);
    let truediff_one = 65_535.0_f64 * pow2_208;
    let mut value = truediff_one / diff;
    let mut target = [0u8; 32];
    for byte in &mut target {
        if value < 1.0 {
            break;
        }
        let low = (value % 256.0) as u8;
        *byte = low;
        // Subtract the low byte before dividing so no libm floor() is needed.
        value = (value - f64::from(low)) / 256.0;
    }
    if value >= 1.0 {
        return [0xFFu8; 32];
    }
    target
}
