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

//! Crate-wide framing error type.

use ii_sv2_types::Error as TypeError;

use crate::header::{ExtType, MessageDescriptor};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FramingError {
    /// Encoder destination buffer is smaller than required.
    BufferTooSmall,
    /// Decoder ran out of bytes parsing header or payload.
    Eof,
    /// Encode-side: `msg_length` would exceed `Header::MAX_LEN`.
    PayloadTooLarge { length: usize },
    /// Header announced more payload than the input slice contains.
    TruncatedPayload { announced: u32, available: usize },
    /// `MessageGroup::dispatch` got a frame from a different extension.
    WrongExtension { expected: ExtType, found: ExtType },
    /// `MessageGroup::dispatch` did not recognize the (msg_type,
    /// is_channel_message) pair within its extension.
    UnknownMessage(MessageDescriptor),
    /// `frame_to_message` got a frame whose descriptor does not match
    /// the target type's `Message::DESCRIPTOR`.
    WrongMessage {
        expected: MessageDescriptor,
        found: MessageDescriptor,
    },
    /// A `types::Decode` impl rejected the frame body.
    Body(TypeError),
    /// The body decoder succeeded but did not consume the whole payload.
    TrailingBytes,
}

impl From<TypeError> for FramingError {
    fn from(e: TypeError) -> Self {
        match e {
            TypeError::Eof => FramingError::Eof,
            TypeError::TrailingBytes => FramingError::TrailingBytes,
            TypeError::BufferTooSmall => FramingError::BufferTooSmall,
            other => FramingError::Body(other),
        }
    }
}
