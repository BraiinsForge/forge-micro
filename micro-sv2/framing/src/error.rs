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
