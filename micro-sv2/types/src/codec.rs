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
