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

//! The schema of the single-message response body that
//! `ubos_http::write_message` writes. It gives that body its place in the
//! API document.

use serde::Serialize;

/// A response body carrying a single human-readable message string.
///
/// Serialized as `{"message":"…"}`: the body of an error, of an endpoint
/// this board does not implement, and of an acknowledgement.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GenericResponse<'response> {
    /// Short description of the outcome.
    pub message: &'response str,
}
