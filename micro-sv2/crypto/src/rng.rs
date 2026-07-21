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

//! Cryptographically-secure entropy source.
//!
//! Implementors plug in the platform's secure random source (OS RNG,
//! hardware RNG on embedded targets, etc.). The trait deliberately
//! exposes only raw bytes — no curve-specific knowledge required.

pub trait Rng {
    /// Fill `dest` with cryptographically secure random bytes.
    ///
    /// The contract is **infallible**: implementors should panic or
    /// block on entropy starvation rather than return short. Embedded
    /// implementations that wrap a hardware RNG should spin until the
    /// peripheral has enough entropy.
    fn fill_bytes(&mut self, dest: &mut [u8]);

    /// Convenience: produce 32 bytes of entropy.
    fn next_256_bits(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        self.fill_bytes(&mut out);
        out
    }
}
