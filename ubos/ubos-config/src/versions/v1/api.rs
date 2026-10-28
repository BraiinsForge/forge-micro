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

//! The HTTP API credentials of version 1.

/// HTTP API credentials.
///
/// `heapless::String<N>` has a `const fn new()` but no const
/// constructor that accepts content, so `ApiConfig::default` is a
/// regular `Default` impl rather than a `const`. The shadow step
/// publishes the value at runtime; no call site needs it at
/// compile time.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ApiConfig {
    /// Login username; maximum 32 bytes.
    pub username: heapless::String<32>,
    /// Login password; maximum 32 bytes.
    pub password: heapless::String<32>,
}
