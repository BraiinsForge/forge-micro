//! SV2 frame header (6 bytes on the wire) and `MessageDescriptor`.

pub type ExtType = u16;
pub type MsgType = u8;

/// Stratum V2 frame header. Six bytes on the wire:
///
/// ```text
///  byte  0 1 2 3 4 5
///       ┌─┬─┬─┬─┬─┬─┐
///       │ E │M│  L  │
///       └─┴─┴─┴─┴─┴─┘
/// ```
///
/// `E` = `extension_field` LE u16; bit 15 is `is_channel_message`,
/// bits 14..=0 are `extension_type`. `M` = `msg_type` (u8). `L` =
/// `msg_length` LE u24.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header {
    pub is_channel_message: bool,
    pub extension_type: ExtType,
    pub msg_type: MsgType,
    pub msg_length: u32,
}

impl Header {
    pub const SIZE: usize = 6;
    pub const MAX_LEN: u32 = 0xff_ffff;

    /// Identity of this header without its length.
    pub const fn descriptor(&self) -> MessageDescriptor {
        MessageDescriptor {
            extension_type: self.extension_type,
            msg_type: self.msg_type,
            is_channel_message: self.is_channel_message,
        }
    }

    /// Build a header for a message with the given descriptor and body length.
    pub const fn for_descriptor(d: MessageDescriptor, msg_length: u32) -> Self {
        Self {
            is_channel_message: d.is_channel_message,
            extension_type: d.extension_type,
            msg_type: d.msg_type,
            msg_length,
        }
    }
}

/// Identifies a message kind without its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageDescriptor {
    pub extension_type: ExtType,
    pub msg_type: MsgType,
    pub is_channel_message: bool,
}

impl MessageDescriptor {
    pub const fn new(
        extension_type: ExtType,
        msg_type: MsgType,
        is_channel_message: bool,
    ) -> Self {
        Self {
            extension_type,
            msg_type,
            is_channel_message,
        }
    }

    pub const fn from_header(h: &Header) -> Self {
        h.descriptor()
    }
}

use ii_sv2_types::{Decode, Encode, Error as TypeError};

impl Encode for Header {
    fn encoded_len(&self) -> usize {
        Self::SIZE
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        if self.msg_length > Self::MAX_LEN {
            return Err(TypeError::OutOfBounds {
                length: self.msg_length as usize,
                min: 0,
                max: Self::MAX_LEN as usize,
            });
        }
        if out.len() < Self::SIZE {
            return Err(TypeError::BufferTooSmall);
        }
        let extension_field =
            self.extension_type | ((self.is_channel_message as u16) << 15);
        out[0..2].copy_from_slice(&extension_field.to_le_bytes());
        out[2] = self.msg_type;
        let len_bytes = self.msg_length.to_le_bytes();
        out[3..6].copy_from_slice(&len_bytes[..3]);
        Ok(Self::SIZE)
    }
}

impl<'de> Decode<'de> for Header {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        let bytes = input.get(..Self::SIZE).ok_or(TypeError::Eof)?;
        let extension_field = u16::from_le_bytes([bytes[0], bytes[1]]);
        let is_channel_message = (extension_field & (1 << 15)) != 0;
        let extension_type = extension_field & 0x7fff;
        let msg_type = bytes[2];
        let mut len_bytes = [0u8; 4];
        len_bytes[..3].copy_from_slice(&bytes[3..6]);
        let msg_length = u32::from_le_bytes(len_bytes);
        *input = &input[Self::SIZE..];
        Ok(Self {
            is_channel_message,
            extension_type,
            msg_type,
            msg_length,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_roundtrip() {
        let d = MessageDescriptor::new(0x12, 0x34, true);
        let h = Header::for_descriptor(d, 100);
        assert_eq!(h.descriptor(), d);
        assert_eq!(MessageDescriptor::from_header(&h), d);
        assert_eq!(h.msg_length, 100);
    }

    #[test]
    fn header_constants() {
        assert_eq!(Header::SIZE, 6);
        assert_eq!(Header::MAX_LEN, 0x00ff_ffff);
    }

    use ii_sv2_types::{Decode, Encode, Error as TypeError};

    #[test]
    fn encode_known_bytes() {
        // ext_type=0x00, msg_type=0x10, msg_length=0x010000, channel=false
        // extension_field = 0x0000 LE => [0x00, 0x00]
        // msg_type        = 0x10
        // msg_length u24  = 0x010000 LE => [0x00, 0x00, 0x01]
        let h = Header {
            is_channel_message: false,
            extension_type: 0x00,
            msg_type: 0x10,
            msg_length: 0x0001_0000,
        };
        let mut out = [0u8; 6];
        let n = h.encode_to(&mut out).unwrap();
        assert_eq!(n, 6);
        assert_eq!(out, [0x00, 0x00, 0x10, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn encode_channel_flag_sets_top_bit() {
        let h = Header {
            is_channel_message: true,
            extension_type: 0x00,
            msg_type: 0x00,
            msg_length: 0,
        };
        let mut out = [0u8; 6];
        h.encode_to(&mut out).unwrap();
        // extension_field = 0x8000 LE => [0x00, 0x80]
        assert_eq!(out, [0x00, 0x80, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn encode_extension_type_low_byte() {
        // Verify ext_type fills the low 15 bits, including byte 0 fully.
        let h = Header {
            is_channel_message: false,
            extension_type: 0x12_34,
            msg_type: 0x00,
            msg_length: 0,
        };
        let mut out = [0u8; 6];
        h.encode_to(&mut out).unwrap();
        assert_eq!(&out[..2], &[0x34, 0x12]);
    }

    #[test]
    fn encode_max_msg_length() {
        let h = Header {
            is_channel_message: false,
            extension_type: 0,
            msg_type: 0,
            msg_length: Header::MAX_LEN,
        };
        let mut out = [0u8; 6];
        h.encode_to(&mut out).unwrap();
        assert_eq!(&out[3..6], &[0xff, 0xff, 0xff]);
    }

    #[test]
    fn encode_rejects_oversized_length() {
        let h = Header {
            is_channel_message: false,
            extension_type: 0,
            msg_type: 0,
            msg_length: Header::MAX_LEN + 1,
        };
        let mut out = [0u8; 6];
        assert_eq!(
            h.encode_to(&mut out),
            Err(TypeError::OutOfBounds {
                length: (Header::MAX_LEN + 1) as usize,
                min: 0,
                max: Header::MAX_LEN as usize,
            })
        );
    }

    #[test]
    fn encode_rejects_short_buffer() {
        let h = Header {
            is_channel_message: false,
            extension_type: 0,
            msg_type: 0,
            msg_length: 0,
        };
        let mut out = [0u8; 5];
        assert_eq!(h.encode_to(&mut out), Err(TypeError::BufferTooSmall));
    }

    #[test]
    fn roundtrip_via_encode_decode() {
        let original = Header {
            is_channel_message: true,
            extension_type: 0x7fff,
            msg_type: 0xab,
            msg_length: 0xcd_ef12,
        };
        let mut buf = [0u8; 6];
        original.encode_to(&mut buf).unwrap();
        let mut cursor: &[u8] = &buf;
        let decoded = Header::decode(&mut cursor).unwrap();
        assert_eq!(decoded, original);
        assert!(cursor.is_empty());
    }

    #[test]
    fn decode_eof_on_short_input() {
        let mut cursor: &[u8] = &[0u8; 5];
        assert_eq!(Header::decode(&mut cursor), Err(TypeError::Eof));
    }
}
