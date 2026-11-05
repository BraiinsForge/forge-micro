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

//! The `{"ok":true}` acknowledgements of the write endpoints.

use serde::Serialize;

/// Acknowledgement of a write that took effect.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Ack {
    /// Always `true`; a failed write answers an error status instead.
    pub ok: bool,
}

/// Acknowledgement of one accepted OTA chunk.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OtaChunkAck {
    /// Always `true`; a rejected chunk answers an error status instead.
    pub ok: bool,
    /// Image offset the next chunk must start at; absent on the final
    /// chunk, which ends the upload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<u32>,
}
