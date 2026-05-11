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

/// Maximum byte length of an [`approx_string`] result. Sized for
/// `"4294901760"` (10 chars) plus the longest sentinel `">MAX"`.
pub const APPROX_STRING_CAP: usize = 12;

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
    let top4 = u32::from_le_bytes(
        target_le[28..32]
            .try_into()
            .expect("BUG: 4-byte slice from a 32-byte array"),
    );
    if top4 != 0 {
        return String::try_from("<1").expect("fits in APPROX_STRING_CAP");
    }
    let next4 = u32::from_le_bytes(
        target_le[24..28]
            .try_into()
            .expect("BUG: 4-byte slice from a 32-byte array"),
    );
    if next4 == 0 {
        return String::try_from(">MAX").expect("fits in APPROX_STRING_CAP");
    }
    let diff = 0xffff_0000_u32 / next4;
    let mut out: String<APPROX_STRING_CAP> = String::new();
    let _ = write!(out, "{diff}");
    out
}

#[cfg(test)]
#[path = "tests/difficulty.rs"]
mod tests;
