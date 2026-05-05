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
}
