//! `Encode` / `Decode<'de>` traits and primitive implementations.

use crate::error::Error;

/// Serialize a value into an SV2 byte stream.
pub trait Encode {
    /// Number of bytes [`encode_to`] will write for this value.
    fn encoded_len(&self) -> usize;

    /// Write the SV2 wire encoding into `out`.
    ///
    /// On success returns the number of bytes written. Returns
    /// [`Error::BufferTooSmall`] if `out.len() < self.encoded_len()`.
    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error>;
}

/// Deserialize a value from an SV2 byte stream.
///
/// `'de` is the lifetime of the underlying payload buffer; borrowed fields in
/// the returned value share that lifetime.
pub trait Decode<'de>: Sized {
    /// Decode one value from the head of `*input`, advancing the cursor past
    /// the consumed bytes. Bytes left untouched stay accessible to the caller
    /// through the same `*input` slice.
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error>;
}

macro_rules! impl_uint {
    ($t:ty, $size:expr) => {
        impl Encode for $t {
            #[inline]
            fn encoded_len(&self) -> usize { $size }

            #[inline]
            fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
                if out.len() < $size {
                    return Err(Error::BufferTooSmall);
                }
                out[..$size].copy_from_slice(&self.to_le_bytes());
                Ok($size)
            }
        }

        impl<'de> Decode<'de> for $t {
            #[inline]
            fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
                let bytes = input.get(..$size).ok_or(Error::Eof)?;
                let arr: [u8; $size] = bytes.try_into().expect("size matches slice");
                *input = &input[$size..];
                Ok(<$t>::from_le_bytes(arr))
            }
        }
    };
}

impl_uint!(u8, 1);
impl_uint!(u16, 2);
impl_uint!(u32, 4);
impl_uint!(u64, 8);
impl_uint!(i8, 1);
impl_uint!(i16, 2);
impl_uint!(i32, 4);
impl_uint!(i64, 8);

impl Encode for f32 {
    #[inline]
    fn encoded_len(&self) -> usize { 4 }
    #[inline]
    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
        self.to_bits().encode_to(out)
    }
}

impl<'de> Decode<'de> for f32 {
    #[inline]
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        Ok(f32::from_bits(u32::decode(input)?))
    }
}

impl Encode for f64 {
    #[inline]
    fn encoded_len(&self) -> usize { 8 }
    #[inline]
    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
        self.to_bits().encode_to(out)
    }
}

impl<'de> Decode<'de> for f64 {
    #[inline]
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        Ok(f64::from_bits(u64::decode(input)?))
    }
}

impl Encode for bool {
    #[inline]
    fn encoded_len(&self) -> usize { 1 }
    #[inline]
    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
        if out.is_empty() { return Err(Error::BufferTooSmall); }
        out[0] = if *self { 1 } else { 0 };
        Ok(1)
    }
}

impl<'de> Decode<'de> for bool {
    #[inline]
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        match u8::decode(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::BadBool),
        }
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encoded_len(&self) -> usize {
        match self {
            None => 1,
            Some(v) => 1 + v.encoded_len(),
        }
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, Error> {
        match self {
            None => {
                if out.is_empty() { return Err(Error::BufferTooSmall); }
                out[0] = 0;
                Ok(1)
            }
            Some(v) => {
                if out.is_empty() { return Err(Error::BufferTooSmall); }
                out[0] = 1;
                let n = v.encode_to(&mut out[1..])?;
                Ok(1 + n)
            }
        }
    }
}

impl<'de, T: Decode<'de>> Decode<'de> for Option<T> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, Error> {
        match u8::decode(input)? {
            0 => Ok(None),
            1 => Ok(Some(T::decode(input)?)),
            _ => Err(Error::BadOptionTag),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: T, expected: &[u8])
    where
        T: Encode + for<'de> Decode<'de> + core::fmt::Debug + PartialEq,
    {
        let mut buf = [0u8; 16];
        let n = value.encode_to(&mut buf).expect("encode_to should succeed");
        assert_eq!(n, value.encoded_len());
        assert_eq!(&buf[..n], expected);

        let mut cursor: &[u8] = &buf[..n];
        let decoded = T::decode(&mut cursor).expect("decode should succeed");
        assert_eq!(decoded, value);
        assert!(cursor.is_empty(), "cursor should be fully consumed");
    }

    #[test]
    fn u8_roundtrip() {
        roundtrip::<u8>(0x00, &[0x00]);
        roundtrip::<u8>(0xa5, &[0xa5]);
        roundtrip::<u8>(0xff, &[0xff]);
    }

    #[test]
    fn u16_roundtrip() {
        roundtrip::<u16>(0x0000, &[0x00, 0x00]);
        roundtrip::<u16>(0xbeef, &[0xef, 0xbe]);
    }

    #[test]
    fn u32_roundtrip() {
        roundtrip::<u32>(0xdead_beef, &[0xef, 0xbe, 0xad, 0xde]);
    }

    #[test]
    fn u64_roundtrip() {
        roundtrip::<u64>(0x0123_4567_89ab_cdef,
            &[0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]);
    }

    #[test]
    fn u32_decode_eof() {
        let mut cursor: &[u8] = &[0x01, 0x02];   // only 2 bytes, need 4
        assert_eq!(u32::decode(&mut cursor), Err(Error::Eof));
    }

    #[test]
    fn i8_roundtrip() {
        roundtrip::<i8>(0, &[0x00]);
        roundtrip::<i8>(-1, &[0xff]);
        roundtrip::<i8>(i8::MIN, &[0x80]);
    }

    #[test]
    fn i16_roundtrip() {
        roundtrip::<i16>(-2, &[0xfe, 0xff]);
        roundtrip::<i16>(i16::MIN, &[0x00, 0x80]);
    }

    #[test]
    fn i32_roundtrip() {
        roundtrip::<i32>(-3, &[0xfd, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn i64_roundtrip() {
        roundtrip::<i64>(-4,
            &[0xfc, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn f32_roundtrip() {
        let mut buf = [0u8; 4];
        let n = 1.0f32.encode_to(&mut buf).unwrap();
        assert_eq!(&buf[..n], &[0x00, 0x00, 0x80, 0x3f]);

        let mut cursor: &[u8] = &buf[..n];
        let decoded = f32::decode(&mut cursor).unwrap();
        assert_eq!(decoded, 1.0f32);
    }

    #[test]
    fn f32_nan_roundtrip() {
        let mut buf = [0u8; 4];
        let n = f32::NAN.encode_to(&mut buf).unwrap();
        let mut cursor: &[u8] = &buf[..n];
        let decoded = f32::decode(&mut cursor).unwrap();
        assert!(decoded.is_nan());
    }

    #[test]
    fn f64_roundtrip() {
        let mut buf = [0u8; 8];
        let n = (-1.5f64).encode_to(&mut buf).unwrap();
        let mut cursor: &[u8] = &buf[..n];
        let decoded = f64::decode(&mut cursor).unwrap();
        assert_eq!(decoded, -1.5f64);
    }

    #[test]
    fn bool_roundtrip() {
        roundtrip::<bool>(false, &[0x00]);
        roundtrip::<bool>(true,  &[0x01]);
    }

    #[test]
    fn bool_decode_bad_value() {
        let mut cursor: &[u8] = &[0x02];
        assert_eq!(bool::decode(&mut cursor), Err(Error::BadBool));
    }

    #[test]
    fn bool_decode_eof() {
        let mut cursor: &[u8] = &[];
        assert_eq!(bool::decode(&mut cursor), Err(Error::Eof));
    }

    #[test]
    fn option_none_roundtrip() {
        roundtrip::<Option<u32>>(None, &[0x00]);
    }

    #[test]
    fn option_some_roundtrip() {
        roundtrip::<Option<u32>>(Some(0xcafe_babe),
            &[0x01, 0xbe, 0xba, 0xfe, 0xca]);
    }

    #[test]
    fn option_decode_bad_tag() {
        let mut cursor: &[u8] = &[0x02];
        assert_eq!(<Option<u32>>::decode(&mut cursor), Err(Error::BadOptionTag));
    }
}
