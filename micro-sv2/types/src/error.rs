//! Crate-wide error type for the borrowed SV2 codec.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Decoder reached end of input before a value was complete.
    Eof,
    /// Top-level decode helper found bytes left over after one value.
    TrailingBytes,
    /// Encoder ran out of room in the destination buffer.
    BufferTooSmall,
    /// A `VarStr` / `VarBytes` length was outside the declared `MIN..=MAX`.
    OutOfBounds { length: usize, min: usize, max: usize },
    /// Bytes claimed to be UTF-8 were not valid.
    Utf8,
    /// A boolean byte was neither 0 nor 1.
    BadBool,
    /// An `Option` tag byte was neither 0 nor 1.
    BadOptionTag,
}
