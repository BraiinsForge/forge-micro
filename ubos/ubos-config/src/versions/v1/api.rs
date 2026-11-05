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

/// HTTP API credentials: the user name and password a client
/// presents.
// The default is built at runtime: a heapless string has no const
// constructor that takes content, and no call site needs the value at
// compile time.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiConfig {
    /// Login username; maximum 32 bytes.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = 32)))]
    pub username: heapless::String<32>,
    /// Login password; maximum 32 bytes.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = 32)))]
    pub password: heapless::String<32>,
}
