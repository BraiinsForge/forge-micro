//! Stratum V2 elementary mining messages — borrowed-first, `no_std`.
//!
//! See `docs/superpowers/specs/2026-05-06-messages-design.md`.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

use ii_sv2_framing::ExtType;

/// SV2 extension byte for the basic mining protocol.
pub const MINING_EXTENSION_TYPE: ExtType = 0x00;

#[cfg(test)]
mod tests {
    // Tests are added by subsequent tasks.
}
