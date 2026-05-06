//! Message-side traits (`Message`, `MessageGroup<'de>`, `IntoFrame`)
//! and the `frame_to_message` helper.

use ii_sv2_types::Encode;

use crate::error::FramingError;
use crate::frame::FrameView;
use crate::header::{ExtType, MessageDescriptor};

/// A typed Stratum V2 message that knows its descriptor.
///
/// Implementors are concrete message structs (e.g.
/// `SetupConnection<'a>`); each declares its
/// `(extension_type, msg_type, is_channel_message)` once.
pub trait Message: Encode {
    const DESCRIPTOR: MessageDescriptor;
}

/// A group of messages that share an SV2 extension namespace.
///
/// Implementors are usually enums whose variants cover every message
/// recognized inside one extension. The implementation maps a
/// `(msg_type, is_channel_message)` pair to the right variant and
/// decodes the payload via `ii_sv2_types::Decode<'de>`.
pub trait MessageGroup<'de>: Sized {
    const EXTENSION_TYPE: ExtType;

    /// Decode one frame into the appropriate variant. Implementations
    /// SHOULD reject frames whose `header.extension_type` does not
    /// equal `Self::EXTENSION_TYPE` with `FramingError::WrongExtension`.
    fn dispatch(frame: FrameView<'de>) -> Result<Self, FramingError>;
}
