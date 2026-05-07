//! Borrowed 256-bit value: a reference to a 32-byte little-endian buffer.

use crate::codec::{Decode, Encode};
use crate::error::Error;

/// Reference to a 256-bit value stored as 32 little-endian bytes.
///
/// `Uint256` borrows from a payload buffer; it does not own the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uint256<'a>(&'a [u8; 32]);

impl<'a> Uint256<'a> {
    /// Construct from an existing 32-byte buffer.
    #[inline]
    pub const fn from_le_bytes(bytes: &'a [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying 32-byte little-endian buffer.
    #[inline]
    pub const fn to_le_bytes(&self) -> &'a [u8; 32] {
        self.0
    }
}

impl<'a> TryFrom<&'a [u8]> for Uint256<'a> {
    type Error = Error;

    fn try_from(s: &'a [u8]) -> Result<Self, Error> {
        let arr: &'a [u8; 32] = s.try_into().map_err(|_| Error::OutOfBounds {
            length: s.len(),
            min: 32,
            max: 32,
        })?;
        Ok(Self(arr))
    }
}

impl<'a> Encode for Uint256<'a> {
    #[inline]
    fn encoded_len(&self) -> usize {
        32
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < 32 {
            return Err(Error::BufferTooSmall);
        }
        out[..32].copy_from_slice(self.0);
        Ok(32)
    }
}

impl<'de> Decode<'de> for Uint256<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        let bytes: &'de [u8] = input.get(..32).ok_or(Error::Eof)?;
        let arr: &'de [u8; 32] = bytes.try_into().expect("len is 32");
        *input = &input[32..];
        Ok(Self(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATTERN: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    #[test]
    fn from_le_bytes_borrows() {
        let v = Uint256::from_le_bytes(&PATTERN);
        assert_eq!(v.to_le_bytes(), &PATTERN);
    }

    #[test]
    fn try_from_slice_exact_len() {
        let v: Uint256 = (&PATTERN[..]).try_into().unwrap();
        assert_eq!(v.to_le_bytes(), &PATTERN);
    }

    #[test]
    fn try_from_slice_too_short() {
        let arr = [0u8; 31];
        let err = Uint256::try_from(&arr[..]).unwrap_err();
        assert_eq!(
            err,
            Error::OutOfBounds {
                length: 31,
                min: 32,
                max: 32
            }
        );
    }

    #[test]
    fn try_from_slice_too_long() {
        let arr = [0u8; 33];
        let err = Uint256::try_from(&arr[..]).unwrap_err();
        assert_eq!(
            err,
            Error::OutOfBounds {
                length: 33,
                min: 32,
                max: 32
            }
        );
    }

    #[test]
    fn encode_decode_roundtrip() {
        let v = Uint256::from_le_bytes(&PATTERN);
        let mut buf = [0u8; 32];
        let n = v.encode_to(&mut buf).unwrap();
        assert_eq!(n, 32);
        assert_eq!(&buf[..], &PATTERN);

        let mut cursor: &[u8] = &buf;
        let decoded = Uint256::decode(&mut cursor).unwrap();
        assert_eq!(decoded.to_le_bytes(), &PATTERN);
        assert!(cursor.is_empty());
    }

    #[test]
    fn decode_eof_when_short() {
        let bytes = [0u8; 31];
        let mut cursor: &[u8] = &bytes;
        assert_eq!(Uint256::decode(&mut cursor), Err(Error::Eof));
    }

    #[test]
    fn decode_borrows_from_input() {
        let bytes: [u8; 32] = PATTERN;
        let mut cursor: &[u8] = &bytes;
        let v = Uint256::decode(&mut cursor).unwrap();
        // The returned reference must point into `bytes`, not a copy.
        assert!(core::ptr::eq(v.to_le_bytes().as_ptr(), bytes.as_ptr()));
    }
}
