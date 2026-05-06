//! `FrameView<'a>` — a borrowed view of `(Header, &'a [u8] payload)`.

use crate::error::FramingError;
use crate::header::Header;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameView<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}

impl<'a> FrameView<'a> {
    /// Construct a view, verifying `payload.len() == header.msg_length`.
    pub fn try_new(header: Header, payload: &'a [u8]) -> Result<Self, FramingError> {
        if payload.len() == header.msg_length as usize {
            Ok(Self { header, payload })
        } else {
            Err(FramingError::TruncatedPayload {
                announced: header.msg_length,
                available: payload.len(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::MessageDescriptor;

    fn h(msg_length: u32) -> Header {
        Header::for_descriptor(MessageDescriptor::new(0, 0, false), msg_length)
    }

    #[test]
    fn try_new_accepts_matching_length() {
        let payload = [1u8, 2, 3];
        let frame = FrameView::try_new(h(3), &payload).unwrap();
        assert_eq!(frame.header.msg_length, 3);
        assert_eq!(frame.payload, &[1, 2, 3]);
    }

    #[test]
    fn try_new_accepts_zero_length() {
        let frame = FrameView::try_new(h(0), &[]).unwrap();
        assert_eq!(frame.payload.len(), 0);
    }

    #[test]
    fn try_new_rejects_short_payload() {
        let payload = [1u8, 2];
        let err = FrameView::try_new(h(3), &payload).unwrap_err();
        assert_eq!(
            err,
            FramingError::TruncatedPayload {
                announced: 3,
                available: 2,
            }
        );
    }

    #[test]
    fn try_new_rejects_long_payload() {
        let payload = [1u8, 2, 3, 4];
        let err = FrameView::try_new(h(3), &payload).unwrap_err();
        assert_eq!(
            err,
            FramingError::TruncatedPayload {
                announced: 3,
                available: 4,
            }
        );
    }
}
