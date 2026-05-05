//! Borrowed, length-bounded SV2 byte sequences.

use crate::error::Error;
use crate::len_prefix::{prefix_len, read_prefix_le, write_prefix_le};
use crate::codec::{Decode, Encode};

/// SV2 length-bounded byte sequence borrowing from a payload buffer.
///
/// `MIN` and `MAX` are inclusive byte lengths. `MAX` must be `<= 0xff_ffff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarBytes<'a, const MIN: usize, const MAX: usize>(&'a [u8]);

pub type Bytes0_32<'a>  = VarBytes<'a, 0, 32>;
pub type Bytes1_32<'a>  = VarBytes<'a, 1, 32>;
pub type Bytes0_255<'a> = VarBytes<'a, 0, 255>;
pub type Bytes1_255<'a> = VarBytes<'a, 1, 255>;
pub type Bytes0_64k<'a> = VarBytes<'a, 0, 0xffff>;
pub type Bytes1_64k<'a> = VarBytes<'a, 1, 0xffff>;
pub type Bytes0_16m<'a> = VarBytes<'a, 0, 0xff_ffff>;

impl<'a, const MIN: usize, const MAX: usize> VarBytes<'a, MIN, MAX> {
    pub(crate) const PREFIX_W: usize = prefix_len(MAX);

    #[inline]
    pub fn as_slice(&self) -> &'a [u8] { self.0 }
}

impl<'a, const MIN: usize, const MAX: usize> TryFrom<&'a [u8]> for VarBytes<'a, MIN, MAX> {
    type Error = Error;

    fn try_from(b: &'a [u8]) -> Result<Self, Error> {
        if (MIN..=MAX).contains(&b.len()) {
            Ok(Self(b))
        } else {
            Err(Error::OutOfBounds { length: b.len(), min: MIN, max: MAX })
        }
    }
}

impl<'a, const MIN: usize, const MAX: usize> core::ops::Deref for VarBytes<'a, MIN, MAX> {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] { self.0 }
}

impl<'a, const MIN: usize, const MAX: usize> AsRef<[u8]> for VarBytes<'a, MIN, MAX> {
    #[inline]
    fn as_ref(&self) -> &[u8] { self.0 }
}

impl<'a, const MIN: usize, const MAX: usize> Encode for VarBytes<'a, MIN, MAX> {
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
        out[pw..total].copy_from_slice(self.0);
        Ok(total)
    }
}

impl<'de, const MIN: usize, const MAX: usize> Decode<'de> for VarBytes<'de, MIN, MAX> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        let pw = Self::PREFIX_W;
        let len = read_prefix_le(input, pw)?;
        if !(MIN..=MAX).contains(&len) {
            return Err(Error::OutOfBounds { length: len, min: MIN, max: MAX });
        }
        let bytes = input.get(..len).ok_or(Error::Eof)?;
        *input = &input[len..];
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: type aliases for VarBytes with `'static` only work on the encode
    // side (encode_to reads `&self`, doesn't tie output lifetime). For decode
    // tests, write `VarBytes::<MIN, MAX>::decode(&mut cursor)` directly so
    // Rust can infer the cursor's lifetime — see VarStr for the precedent.
    type B0_32 = VarBytes<'static, 0, 32>;
    type B1_32 = VarBytes<'static, 1, 32>;

    #[test]
    fn try_from_accepts_within_bounds() {
        let b: B0_32 = (&b""[..]).try_into().unwrap();
        assert_eq!(b.as_slice(), b"");

        let b: B0_32 = (&b"abc"[..]).try_into().unwrap();
        assert_eq!(b.as_slice(), b"abc");
    }

    #[test]
    fn try_from_rejects_below_min() {
        let err = B1_32::try_from(&b""[..]).unwrap_err();
        assert_eq!(err, Error::OutOfBounds { length: 0, min: 1, max: 32 });
    }

    #[test]
    fn try_from_rejects_above_max() {
        let arr = [0u8; 33];
        let err = VarBytes::<0, 32>::try_from(&arr[..]).unwrap_err();
        assert_eq!(err, Error::OutOfBounds { length: 33, min: 0, max: 32 });
    }

    #[test]
    fn encode_decode_roundtrip_short() {
        let b: B0_32 = (&[1u8, 2, 3][..]).try_into().unwrap();
        let mut buf = [0u8; 16];
        let n = b.encode_to(&mut buf).unwrap();
        assert_eq!(&buf[..n], &[0x03, 0x01, 0x02, 0x03]);

        let mut cursor: &[u8] = &buf[..n];
        let decoded = VarBytes::<0, 32>::decode(&mut cursor).unwrap();
        assert_eq!(decoded.as_slice(), &[1, 2, 3]);
        assert!(cursor.is_empty());
    }

    #[test]
    fn decode_rejects_above_max() {
        let bytes = [0x21u8];
        let mut cursor: &[u8] = &bytes;
        assert_eq!(
            VarBytes::<0, 32>::decode(&mut cursor),
            Err(Error::OutOfBounds { length: 33, min: 0, max: 32 }),
        );
    }

    #[test]
    fn decode_eof_when_payload_short() {
        let bytes = [0x05u8, 1, 2];
        let mut cursor: &[u8] = &bytes;
        assert_eq!(VarBytes::<0, 32>::decode(&mut cursor), Err(Error::Eof));
    }
}
