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

//! The seeds a board draws at boot, in one shape for every platform
//! crate that builds it. The consumers are platform-neutral: the network
//! stack, the session CSPRNG and the SV2 noise handshake each take one
//! field, so a platform crate only fills it in, from whatever entropy
//! source the chip has. The RP family builds it today. The ESP32-S3
//! still draws the three seeds as separate locals.

/// The three seeds a board draws at boot.
pub struct Seeds {
    /// The embassy-net stack seed.
    pub net: u64,
    /// The session CSPRNG seed.
    pub csprng: [u8; 32],
    /// The SV2 noise handshake seed.
    pub sv2: [u8; 32],
}
