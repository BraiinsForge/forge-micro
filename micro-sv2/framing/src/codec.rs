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

//! `FrameCodec` trait and the `PlaintextCodec` reference impl.

use ii_sv2_types::{Decode, Encode};

use crate::error::FramingError;
use crate::frame::FrameView;
use crate::header::Header;
use crate::message::IntoFrame;

pub trait FrameCodec {
    type Error: From<FramingError>;
    const HEADER_WIRE_SIZE: usize;

    /// Bytes for the payload chunk(s) on the wire given a plaintext
    /// `body_len`. Pure function of `body_len`: no `&self`.
    fn payload_wire_size(body_len: usize) -> usize;

    fn encode<F: IntoFrame>(&mut self, frame: &F, out: &mut [u8]) -> Result<usize, Self::Error>;

    fn decode_header(&mut self, buf: &mut [u8]) -> Result<Header, Self::Error>;

    fn decode_frame<'a>(
        &mut self,
        header: Header,
        buf: &'a mut [u8],
    ) -> Result<FrameView<'a>, Self::Error>;
}

/// Zero-state `FrameCodec` that performs no transformation.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlaintextCodec;

impl FrameCodec for PlaintextCodec {
    type Error = FramingError;
    const HEADER_WIRE_SIZE: usize = Header::SIZE;

    fn payload_wire_size(body_len: usize) -> usize {
        body_len
    }

    fn encode<F: IntoFrame>(&mut self, frame: &F, out: &mut [u8]) -> Result<usize, Self::Error> {
        let body_len = frame.body_len();
        if body_len > Header::MAX_LEN as usize {
            return Err(FramingError::PayloadTooLarge { length: body_len });
        }
        let total = Self::HEADER_WIRE_SIZE + Self::payload_wire_size(body_len);
        if out.len() < total {
            return Err(FramingError::BufferTooSmall);
        }
        let header = frame.header();
        header.encode_to(&mut out[..Header::SIZE])?;
        frame.write_body(&mut out[Header::SIZE..total])?;
        Ok(total)
    }

    fn decode_header(&mut self, buf: &mut [u8]) -> Result<Header, Self::Error> {
        if buf.len() < Self::HEADER_WIRE_SIZE {
            return Err(FramingError::Eof);
        }
        let mut cursor: &[u8] = &buf[..Header::SIZE];
        Header::decode(&mut cursor).map_err(FramingError::from)
    }

    fn decode_frame<'a>(
        &mut self,
        header: Header,
        buf: &'a mut [u8],
    ) -> Result<FrameView<'a>, Self::Error> {
        let payload_len = header.msg_length as usize;
        if buf.len() < payload_len {
            return Err(FramingError::TruncatedPayload {
                announced: header.msg_length,
                available: buf.len(),
            });
        }
        FrameView::try_new(header, &buf[..payload_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::MessageDescriptor;
    use crate::message::Message;
    use ii_sv2_types::Error as TypeError;

    /// Same fixture as in `message.rs`: u32 body, descriptor (0xff, 0x10, false).
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
        const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(0xff, 0x10, false);
    }

    #[test]
    fn encode_msg_byte_exact() {
        let mut codec = PlaintextCodec;
        let msg = TestMsg { x: 0xdead_beef };
        let mut out = [0u8; 32];
        let n = codec.encode(&msg, &mut out).unwrap();
        assert_eq!(n, 10); // 6 header + 4 body
        // Header: ext_field=0x00ff_LE, msg_type=0x10, msg_length=4_LE_u24
        assert_eq!(
            &out[..n],
            &[
                0xff, 0x00, 0x10, 0x04, 0x00, 0x00, // header
                0xef, 0xbe, 0xad, 0xde, // body
            ]
        );
    }

    #[test]
    fn encode_buffer_too_small() {
        let mut codec = PlaintextCodec;
        let msg = TestMsg { x: 0 };
        let mut out = [0u8; 9];
        assert_eq!(
            codec.encode(&msg, &mut out),
            Err(FramingError::BufferTooSmall)
        );
    }

    #[test]
    fn decode_header_then_decode_frame_roundtrip() {
        let mut codec = PlaintextCodec;
        let msg = TestMsg { x: 0x0123_4567 };

        let mut wire = [0u8; 32];
        let n = codec.encode(&msg, &mut wire).unwrap();
        let _ = n;

        // Receiver side: copy header bytes, decode header.
        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert_eq!(header.descriptor(), TestMsg::DESCRIPTOR);
        assert_eq!(header.msg_length, 4);

        // Payload phase.
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        assert_eq!(pn, 4);
        let mut pbuf = [0u8; 32];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        assert_eq!(frame.payload, &[0x67, 0x45, 0x23, 0x01]);

        // Zero-copy check: the FrameView's payload pointer must alias `pbuf`.
        assert!(core::ptr::eq(frame.payload.as_ptr(), pbuf.as_ptr()));
    }

    #[test]
    fn decode_header_rejects_short_buffer() {
        let mut codec = PlaintextCodec;
        let mut buf = [0u8; 5];
        assert_eq!(codec.decode_header(&mut buf), Err(FramingError::Eof));
    }

    #[test]
    fn decode_frame_rejects_short_payload() {
        let mut codec = PlaintextCodec;
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 4);
        let mut pbuf = [0u8; 2];
        assert_eq!(
            codec.decode_frame(header, &mut pbuf),
            Err(FramingError::TruncatedPayload {
                announced: 4,
                available: 2,
            })
        );
    }

    #[test]
    fn encode_view_matches_encode_msg() {
        let mut codec = PlaintextCodec;
        let msg = TestMsg { x: 0xdead_beef };

        // Encode via Message.
        let mut buf_msg = [0u8; 32];
        let nm = codec.encode(&msg, &mut buf_msg).unwrap();

        // Build a FrameView from the wire body and re-encode it; bytes
        // should match exactly.
        let body = 0xdead_beef_u32.to_le_bytes();
        let header = Header::for_descriptor(TestMsg::DESCRIPTOR, 4);
        let view = FrameView::try_new(header, &body).unwrap();
        let mut buf_view = [0u8; 32];
        let nv = codec.encode(&view, &mut buf_view).unwrap();

        assert_eq!(nm, nv);
        assert_eq!(&buf_msg[..nm], &buf_view[..nv]);
    }

    #[test]
    fn encode_payload_too_large() {
        // Build an in-test type that lies about its body_len.
        struct BigFrame;
        impl IntoFrame for BigFrame {
            fn descriptor(&self) -> MessageDescriptor {
                TestMsg::DESCRIPTOR
            }
            fn body_len(&self) -> usize {
                (Header::MAX_LEN as usize) + 1
            }
            fn write_body(&self, _out: &mut [u8]) -> Result<(), FramingError> {
                unreachable!("encode rejects before write_body")
            }
        }
        let mut codec = PlaintextCodec;
        let mut out = [0u8; 64];
        let err = codec.encode(&BigFrame, &mut out).unwrap_err();
        assert_eq!(
            err,
            FramingError::PayloadTooLarge {
                length: (Header::MAX_LEN as usize) + 1,
            }
        );
    }
}
