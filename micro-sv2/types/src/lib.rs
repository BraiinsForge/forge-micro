//! Borrowed-first Stratum V2 type vocabulary for `no_std` environments.
//!
//! See `docs/superpowers/specs/2026-05-05-types-borrowed-design.md`.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

mod codec;
mod error;
mod len_prefix;
mod uint256;
mod var_bytes;
mod var_str;

pub use crate::error::Error;
pub use crate::codec::{Decode, Encode, encode_to_slice, from_slice};
