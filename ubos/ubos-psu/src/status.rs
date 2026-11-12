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

//! The fault flags a supply reports.

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;

/// PSU status flags decoded from the `TPS6287x` STATUS register.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
// Mirrors a hardware status byte; collapsing the bits into a state
// machine would lose the 1:1 mapping to the register.
#[allow(clippy::struct_excessive_bools)]
pub struct StatusFlags {
    pub current_limit: bool,
    pub thermal_shutdown: bool,
    pub overvoltage: bool,
    pub undervoltage: bool,
}

impl StatusFlags {
    /// The flags set in either value.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            current_limit: self.current_limit || other.current_limit,
            thermal_shutdown: self.thermal_shutdown || other.thermal_shutdown,
            overvoltage: self.overvoltage || other.overvoltage,
            undervoltage: self.undervoltage || other.undervoltage,
        }
    }
}
