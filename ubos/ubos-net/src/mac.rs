//! Build a stable MAC address from a per-board chip-unique identifier.
//!
//! The `WIZnet` chips ship with a blank hardware-address register, so the
//! firmware must supply the six MAC octets. Rather than a compile-time
//! constant baked per build, [`derive_mac`] folds a chip-unique ID into
//! the address so every board self-assigns a stable, distinct MAC with no
//! per-unit build flags.
//!
//! The layout mirrors a real IEEE assignment so the move to a registered
//! block later is a one-constant swap:
//!
//! ```text
//! byte:   0     1     2       3         4        5
//!       [ ---- OUI ---- ][ family ][ hash_hi  hash_lo ]
//! ```
//!
//! The top three bytes are the OUI. [`DUMMY_OUI`] is a placeholder with
//! the locally-administered bit set so it cannot collide with any
//! registered vendor; productization replaces it with the purchased OUI,
//! whose locally-administered bit is clear. Byte 3 identifies the board
//! family. The last two bytes are a 16-bit hash of the chip-unique ID,
//! which disambiguates units within a family.

/// Placeholder organisationally-unique identifier for lab builds: the
/// ASCII bytes `JKR`.
///
/// The first octet `J` (`0x4A`) has the locally-administered bit (`0x02`)
/// set and the multicast bit clear, so the address is a valid unicast
/// LAA. Because every registered OUI has the locally-administered bit
/// clear, this can never collide with a real vendor block. Replace all
/// three bytes with the IEEE-assigned OUI when the boards ship.
pub const DUMMY_OUI: [u8; 3] = *b"JKR";

/// FNV-1a offset basis (32-bit).
const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;

/// FNV-1a prime (32-bit).
const FNV_PRIME: u32 = 0x0100_0193;

/// Hash bytes into two octets with FNV-1a, xor-folded from the 32-bit
/// state.
///
/// Xor-folding the two halves mixes the high bits that the truncated
/// low 16 bits would otherwise discard, per the FNV recommendation for
/// outputs narrower than the native width. The folded value fits in 16
/// bits, so the top two big-endian bytes are zero and are dropped.
fn fnv1a_16(data: &[u8]) -> [u8; 2] {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in data {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let folded = ((hash >> 16) ^ (hash & 0xffff)).to_be_bytes();
    [folded[2], folded[3]]
}

/// Build a 6-byte MAC from an OUI, a board-family byte, and a chip uid.
///
/// The result is deterministic: the same `uid` always yields the same
/// address, so a board keeps its MAC across reboots and reflashes. The
/// uid may be any length; only its hash enters the address.
#[must_use]
pub fn derive_mac(oui: [u8; 3], family: u8, uid: &[u8]) -> [u8; 6] {
    let [hash_hi, hash_lo] = fnv1a_16(uid);
    [oui[0], oui[1], oui[2], family, hash_hi, hash_lo]
}

#[cfg(test)]
#[path = "tests/mac.rs"]
mod tests;
