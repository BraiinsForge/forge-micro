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

//! `ExtensionFrame` — one frame of a Stratum V2 extension the mining
//! session carries without reading it.

/// One frame of a Stratum V2 extension other than mining, as the
/// adapter received it or as a caller asks the adapter to send it. The
/// adapter never interprets the payload, so a protocol that rides a
/// mining session stays with the code that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ExtensionFrame<'a> {
    /// The extension the frame belongs to.
    pub extension_type: u16,
    /// The message type inside that extension.
    pub msg_type: u8,
    /// Whether the message addresses a channel, as stated by its descriptor.
    pub is_channel_message: bool,
    /// The message body, passed through as received or as given.
    pub payload: &'a [u8],
}

#[cfg(test)]
#[path = "tests/extension.rs"]
mod tests;
