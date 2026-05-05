//! Length-prefix encoding helpers shared by `VarStr` and `VarBytes`.

use crate::error::Error;

/// Width in bytes of the length prefix used for a sequence whose maximum
/// length is `max`. SV2 uses 1, 2, or 3 byte little-endian prefixes
/// (corresponding to U8, U16, and U24 in the spec).
///
/// Panics in const context if `max > 0xff_ffff`. This is intentional: any
/// `MAX` outside the supported range will fail at monomorphization time
/// when the result is consumed in a `const` item or `const { ... }` block.
pub(crate) const fn prefix_len(max: usize) -> usize {
    match max {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        0x1_0000..=0xff_ffff => 3,
        _ => panic!("VarStr/VarBytes MAX must be <= 0xff_ffff"),
    }
}

/// Write a length prefix of `width` bytes (1, 2, or 3) little-endian.
pub(crate) fn write_prefix_le(value: usize, width: usize, out: &mut [u8])
    -> Result<usize, Error>
{
    debug_assert!((1..=3).contains(&width), "prefix width must be 1, 2, or 3");
    if out.len() < width {
        return Err(Error::BufferTooSmall);
    }
    let bytes = (value as u32).to_le_bytes();
    out[..width].copy_from_slice(&bytes[..width]);
    Ok(width)
}

/// Read a length prefix of `width` bytes (1, 2, or 3) little-endian and
/// advance the cursor.
pub(crate) fn read_prefix_le(input: &mut &[u8], width: usize)
    -> Result<usize, Error>
{
    debug_assert!((1..=3).contains(&width), "prefix width must be 1, 2, or 3");
    let bytes = input.get(..width).ok_or(Error::Eof)?;
    let mut buf = [0u8; 4];
    buf[..width].copy_from_slice(bytes);
    *input = &input[width..];
    Ok(u32::from_le_bytes(buf) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_len_dispatch() {
        assert_eq!(prefix_len(0x00), 1);
        assert_eq!(prefix_len(0xff), 1);
        assert_eq!(prefix_len(0x100), 2);
        assert_eq!(prefix_len(0xffff), 2);
        assert_eq!(prefix_len(0x1_0000), 3);
        assert_eq!(prefix_len(0xff_ffff), 3);
    }

    #[test]
    fn write_and_read_1byte_prefix() {
        let mut buf = [0u8; 4];
        let w = write_prefix_le(0x7a, 1, &mut buf).unwrap();
        assert_eq!(w, 1);
        assert_eq!(&buf[..1], &[0x7a]);

        let mut cursor: &[u8] = &buf[..1];
        let v = read_prefix_le(&mut cursor, 1).unwrap();
        assert_eq!(v, 0x7a);
        assert!(cursor.is_empty());
    }

    #[test]
    fn write_and_read_2byte_prefix() {
        let mut buf = [0u8; 4];
        let w = write_prefix_le(0xbeef, 2, &mut buf).unwrap();
        assert_eq!(w, 2);
        assert_eq!(&buf[..2], &[0xef, 0xbe]);

        let mut cursor: &[u8] = &buf[..2];
        let v = read_prefix_le(&mut cursor, 2).unwrap();
        assert_eq!(v, 0xbeef);
    }

    #[test]
    fn write_and_read_3byte_prefix() {
        let mut buf = [0u8; 4];
        let w = write_prefix_le(0xab_cdef, 3, &mut buf).unwrap();
        assert_eq!(w, 3);
        assert_eq!(&buf[..3], &[0xef, 0xcd, 0xab]);

        let mut cursor: &[u8] = &buf[..3];
        let v = read_prefix_le(&mut cursor, 3).unwrap();
        assert_eq!(v, 0xab_cdef);
    }

    #[test]
    fn write_prefix_buffer_too_small() {
        let mut buf = [0u8; 1];
        assert_eq!(write_prefix_le(0xbeef, 2, &mut buf), Err(Error::BufferTooSmall));
    }

    #[test]
    fn read_prefix_eof() {
        let mut cursor: &[u8] = &[0x12];
        assert_eq!(read_prefix_le(&mut cursor, 2), Err(Error::Eof));
    }
}
