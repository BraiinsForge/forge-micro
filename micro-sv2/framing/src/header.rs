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
}
