//! Hex-decode helper used by the V1 adapter to turn wire-form
//! `&str` slices (`prev_hash`, coinbase parts, branches) into
//! binary before they reach the visitor.
//!
//! Lives here rather than in `mining-protocol` because hex is a V1
//! wire-encoding detail; V2 sees binary directly and never calls
//! into this module.

#[cfg(test)]
#[path = "tests/hex.rs"]
mod tests;

/// Decode `hex` into `bytes` byte-for-byte.
///
/// Returns `false` if `hex.len() != bytes.len() * 2` or if any byte
/// of `hex` is not an ASCII hex digit. On `false`, `bytes` may have
/// been partially overwritten — callers reading the buffer after a
/// failure must treat its contents as undefined.
#[must_use]
pub fn decode_hex(hex: &[u8], bytes: &mut [u8]) -> bool {
    if hex.len() != bytes.len() * 2 {
        return false;
    }
    for index in 0..bytes.len() {
        let high = hex_nibble(hex[index * 2]);
        let low = hex_nibble(hex[index * 2 + 1]);
        match (high, low) {
            (Some(high_val), Some(low_val)) => bytes[index] = (high_val << 4) | low_val,
            _ => return false,
        }
    }
    true
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
