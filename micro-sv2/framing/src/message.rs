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

use crate::header::Header;

/// Anything the codec can encode as a frame: descriptor + body length
/// + body bytes.
///
/// Users rarely implement this directly: every `Message` gets a
/// blanket impl, and `FrameView<'a>` has a direct impl for forwarders
/// / relays. Implement it on a custom type only if you have a frame
/// source that fits neither pattern.
pub trait IntoFrame {
    fn descriptor(&self) -> MessageDescriptor;
    fn body_len(&self) -> usize;
    fn write_body(&self, out: &mut [u8]) -> Result<(), FramingError>;

    /// Convenience: build the wire header from `descriptor` + `body_len`.
    fn header(&self) -> Header {
        Header::for_descriptor(self.descriptor(), self.body_len() as u32)
    }
}

impl<M: Message> IntoFrame for M {
    fn descriptor(&self) -> MessageDescriptor {
        M::DESCRIPTOR
    }

    fn body_len(&self) -> usize {
        self.encoded_len()
    }

    fn write_body(&self, out: &mut [u8]) -> Result<(), FramingError> {
        self.encode_to(out).map(drop).map_err(FramingError::from)
    }
}

impl<'a> IntoFrame for FrameView<'a> {
    fn descriptor(&self) -> MessageDescriptor {
        self.header.descriptor()
    }

    fn body_len(&self) -> usize {
        self.payload.len()
    }

    fn write_body(&self, out: &mut [u8]) -> Result<(), FramingError> {
        if out.len() < self.payload.len() {
            return Err(FramingError::BufferTooSmall);
        }
        out[..self.payload.len()].copy_from_slice(self.payload);
        Ok(())
    }
}

use ii_sv2_types::Decode;

/// Caller-knows-the-type decode: turn a frame's body into a concrete
/// `Message`, validating the descriptor and consuming the entire payload.
pub fn frame_to_message<'de, M>(frame: FrameView<'de>) -> Result<M, FramingError>
where
    M: Message + Decode<'de>,
{
    if frame.header.descriptor() != M::DESCRIPTOR {
        return Err(FramingError::WrongMessage {
            expected: M::DESCRIPTOR,
            found: frame.header.descriptor(),
        });
    }
    let mut cursor: &'de [u8] = frame.payload;
    let msg = M::decode(&mut cursor).map_err(FramingError::from)?;
    if !cursor.is_empty() {
        return Err(FramingError::TrailingBytes);
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Header;
    use ii_sv2_types::{Decode, Encode, Error as TypeError};

    /// Tiny in-test message: u32 body. ext_type=0xff, msg_type=0x10.
    #[derive(Debug, PartialEq)]
    struct TestMsg {
        x: u32,
    }

    impl Encode for TestMsg {
        fn encoded_len(&self) -> usize {
            self.x.encoded_len()
        }
        fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
            self.x.encode_to(out)
        }
    }

    impl<'de> Decode<'de> for TestMsg {
        fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
            Ok(Self {
                x: u32::decode(input)?,
            })
        }
    }

    impl Message for TestMsg {
        const DESCRIPTOR: MessageDescriptor =
            MessageDescriptor::new(0xff, 0x10, false);
    }

    #[test]
    fn intoframe_for_message_uses_descriptor() {
        let msg = TestMsg { x: 0xdead_beef };
        let descriptor = IntoFrame::descriptor(&msg);
        assert_eq!(descriptor, TestMsg::DESCRIPTOR);
        assert_eq!(IntoFrame::body_len(&msg), 4);
    }

    #[test]
    fn intoframe_for_message_writes_body() {
        let msg = TestMsg { x: 0xdead_beef };
        let mut out = [0u8; 8];
        IntoFrame::write_body(&msg, &mut out[..4]).unwrap();
        assert_eq!(&out[..4], &[0xef, 0xbe, 0xad, 0xde]);
    }

    #[test]
    fn intoframe_for_message_header_uses_descriptor_plus_body_len() {
        let msg = TestMsg { x: 0 };
        let h = IntoFrame::header(&msg);
        assert_eq!(h.descriptor(), TestMsg::DESCRIPTOR);
        assert_eq!(h.msg_length, 4);
    }

    #[test]
    fn intoframe_for_message_write_body_buffer_too_small() {
        let msg = TestMsg { x: 0 };
        let mut out = [0u8; 2];
        let err = IntoFrame::write_body(&msg, &mut out).unwrap_err();
        assert_eq!(err, FramingError::BufferTooSmall);
    }

    #[test]
    fn intoframe_for_frameview_round_trips() {
        let payload = [1u8, 2, 3];
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 3);
        let frame = FrameView::try_new(header, &payload).unwrap();

        assert_eq!(IntoFrame::descriptor(&frame), TestMsg::DESCRIPTOR);
        assert_eq!(IntoFrame::body_len(&frame), 3);

        let mut out = [0u8; 4];
        IntoFrame::write_body(&frame, &mut out[..3]).unwrap();
        assert_eq!(&out[..3], &[1, 2, 3]);
    }

    #[test]
    fn intoframe_for_frameview_buffer_too_small() {
        let payload = [1u8, 2, 3];
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 3);
        let frame = FrameView::try_new(header, &payload).unwrap();
        let mut out = [0u8; 2];
        let err = IntoFrame::write_body(&frame, &mut out).unwrap_err();
        assert_eq!(err, FramingError::BufferTooSmall);
    }

    #[test]
    fn frame_to_message_happy_path() {
        // Build a frame with the right descriptor and a u32 body.
        let body = 0xdead_beef_u32.to_le_bytes();
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 4);
        let frame = FrameView::try_new(header, &body).unwrap();
        let msg: TestMsg = frame_to_message(frame).unwrap();
        assert_eq!(msg, TestMsg { x: 0xdead_beef });
    }

    #[test]
    fn frame_to_message_wrong_descriptor() {
        // ext_type matches but msg_type doesn't.
        let body = 0u32.to_le_bytes();
        let other = MessageDescriptor::new(0xff, 0x11, false);
        let header = Header::for_descriptor(other, 4);
        let frame = FrameView::try_new(header, &body).unwrap();
        let err = frame_to_message::<TestMsg>(frame).unwrap_err();
        assert_eq!(
            err,
            FramingError::WrongMessage {
                expected: TestMsg::DESCRIPTOR,
                found: other,
            }
        );
    }

    #[test]
    fn frame_to_message_trailing_bytes() {
        // Body has 5 bytes but TestMsg only consumes 4.
        let body = [0u8, 0, 0, 0, 0xff];
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 5);
        let frame = FrameView::try_new(header, &body).unwrap();
        let err = frame_to_message::<TestMsg>(frame).unwrap_err();
        assert_eq!(err, FramingError::TrailingBytes);
    }

    #[test]
    fn frame_to_message_body_decode_eof() {
        // Body has only 2 bytes; TestMsg's u32 needs 4.
        let body = [0u8, 0];
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 2);
        let frame = FrameView::try_new(header, &body).unwrap();
        let err = frame_to_message::<TestMsg>(frame).unwrap_err();
        assert_eq!(err, FramingError::Eof);
    }
}
