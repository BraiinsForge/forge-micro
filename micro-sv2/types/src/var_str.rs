//! Borrowed, length-bounded SV2 strings.

use crate::error::Error;
use crate::len_prefix::{prefix_len, read_prefix_le, write_prefix_le};
use crate::codec::{Decode, Encode};

/// SV2 length-bounded UTF-8 string borrowing from a payload buffer.
///
/// `MIN` and `MAX` are inclusive byte lengths. `MAX` must be `<= 0xff_ffff`
/// (24-bit prefix is the largest SV2 supports).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarStr<'a, const MIN: usize, const MAX: usize>(&'a str);

pub type Str0_32<'a>  = VarStr<'a, 0, 32>;
pub type Str1_32<'a>  = VarStr<'a, 1, 32>;
pub type Str0_255<'a> = VarStr<'a, 0, 255>;
pub type Str1_255<'a> = VarStr<'a, 1, 255>;
pub type Str0_64k<'a> = VarStr<'a, 0, 0xffff>;

impl<'a, const MIN: usize, const MAX: usize> VarStr<'a, MIN, MAX> {
    /// Compile-time width of the wire length prefix. Referencing this
    /// associated const triggers a monomorphization-time error if `MAX`
    /// exceeds the supported range.
    pub(crate) const PREFIX_W: usize = prefix_len(MAX);

    #[inline]
    pub fn as_str(&self) -> &'a str { self.0 }
}

impl<'a, const MIN: usize, const MAX: usize> TryFrom<&'a str> for VarStr<'a, MIN, MAX> {
    type Error = Error;

    fn try_from(s: &'a str) -> Result<Self, Error> {
        if (MIN..=MAX).contains(&s.len()) {
            Ok(Self(s))
        } else {
            Err(Error::OutOfBounds { length: s.len(), min: MIN, max: MAX })
        }
    }
}

impl<'a, const MIN: usize, const MAX: usize> core::ops::Deref for VarStr<'a, MIN, MAX> {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str { self.0 }
}

impl<'a, const MIN: usize, const MAX: usize> AsRef<str> for VarStr<'a, MIN, MAX> {
    #[inline]
    fn as_ref(&self) -> &str { self.0 }
}

impl<'a, const MIN: usize, const MAX: usize> Encode for VarStr<'a, MIN, MAX> {
    fn encoded_len(&self) -> usize {
        Self::PREFIX_W + self.0.len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
        let total = self.encoded_len();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let pw = Self::PREFIX_W;
        write_prefix_le(self.0.len(), pw, out)?;
        out[pw..total].copy_from_slice(self.0.as_bytes());
        Ok(total)
    }
}

impl<'de, const MIN: usize, const MAX: usize> Decode<'de> for VarStr<'de, MIN, MAX> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        let pw = Self::PREFIX_W;
        let len = read_prefix_le(input, pw)?;
        if !(MIN..=MAX).contains(&len) {
            return Err(Error::OutOfBounds { length: len, min: MIN, max: MAX });
        }
        let bytes = input.get(..len).ok_or(Error::Eof)?;
        let s = core::str::from_utf8(bytes).map_err(|_| Error::Utf8)?;
        *input = &input[len..];
        Ok(Self(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type S0_32 = VarStr<'static, 0, 32>;
    type S1_32 = VarStr<'static, 1, 32>;

    #[test]
    fn try_from_accepts_within_bounds() {
        let s: S0_32 = "".try_into().unwrap();
        assert_eq!(s.as_str(), "");

        let s: S0_32 = "hello".try_into().unwrap();
        assert_eq!(s.as_str(), "hello");

        let max: S0_32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ123456".try_into().unwrap();
        assert_eq!(max.as_str().len(), 32);
    }

    #[test]
    fn try_from_rejects_below_min() {
        let err = S1_32::try_from("").unwrap_err();
        assert_eq!(err, Error::OutOfBounds { length: 0, min: 1, max: 32 });
    }

    #[test]
    fn try_from_rejects_above_max() {
        let s: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567"; // 33 chars
        let err = S0_32::try_from(s).unwrap_err();
        assert_eq!(err, Error::OutOfBounds { length: 33, min: 0, max: 32 });
    }

    #[test]
    fn deref_to_str() {
        let s: S0_32 = "abc".try_into().unwrap();
        let r: &str = &s;
        assert_eq!(r, "abc");
    }

    #[test]
    fn encode_decode_roundtrip_short() {
        let s: S0_32 = "abc".try_into().unwrap();
        let mut buf = [0u8; 16];
        let n = s.encode_to(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..n], &[0x03, b'a', b'b', b'c']);

        let mut cursor: &[u8] = &buf[..n];
        let decoded = VarStr::<0, 32>::decode(&mut cursor).unwrap();
        assert_eq!(decoded.as_str(), "abc");
        assert!(cursor.is_empty());
    }

    #[test]
    fn encode_decode_roundtrip_2byte_prefix() {
        let s: VarStr<0, 0xffff> = "abc".try_into().unwrap();
        let mut buf = [0u8; 16];
        let n = s.encode_to(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], &[0x03, 0x00, b'a', b'b', b'c']);

        let mut cursor: &[u8] = &buf[..n];
        let decoded = VarStr::<0, 0xffff>::decode(&mut cursor).unwrap();
        assert_eq!(decoded.as_str(), "abc");
    }

    #[test]
    fn encode_buffer_too_small() {
        let s: S0_32 = "abc".try_into().unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(s.encode_to(&mut buf), Err(Error::BufferTooSmall));
    }

    #[test]
    fn decode_rejects_bad_utf8() {
        let bytes = [0x02u8, 0xff, 0xff];
        let mut cursor: &[u8] = &bytes;
        assert_eq!(VarStr::<0, 32>::decode(&mut cursor), Err(Error::Utf8));
    }

    #[test]
    fn decode_rejects_below_min() {
        let bytes = [0x00u8];
        let mut cursor: &[u8] = &bytes;
        assert_eq!(
            VarStr::<1, 32>::decode(&mut cursor),
            Err(Error::OutOfBounds { length: 0, min: 1, max: 32 }),
        );
    }

    #[test]
    fn decode_rejects_above_max() {
        let bytes = [0x21u8]; // length 33 in 1-byte prefix
        let mut cursor: &[u8] = &bytes;
        assert_eq!(
            VarStr::<0, 32>::decode(&mut cursor),
            Err(Error::OutOfBounds { length: 33, min: 0, max: 32 }),
        );
    }

    #[test]
    fn decode_eof_when_payload_short() {
        let bytes = [0x05u8, b'a', b'b']; // claims 5 bytes, only 2 follow
        let mut cursor: &[u8] = &bytes;
        assert_eq!(VarStr::<0, 32>::decode(&mut cursor), Err(Error::Eof));
    }
}
