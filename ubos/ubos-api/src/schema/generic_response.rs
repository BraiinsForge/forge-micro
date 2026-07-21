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

//! Generic single-message response body used by 501 stubs and, in a
//! later phase, by action-endpoint acknowledgements.

use serde::Serialize;

/// A response body carrying a single human-readable message string.
///
/// Serialized as `{"message":"…"}`. Used wherever the firmware cannot
/// or does not perform the requested operation and needs to explain
/// why in a machine-readable envelope.
#[derive(Serialize)]
pub struct GenericResponse<'response> {
    /// Short description of the outcome.
    pub message: &'response str,
}
