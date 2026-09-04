// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Borrowed-first Stratum V2 type vocabulary for `no_std` environments.
//!
//! Holds the byte-level codec trait pair [`Encode`]/[`Decode`] with the
//! [`encode_to_slice`]/[`from_slice`] helpers, and the bounded wire
//! types every message field is built from: [`Uint256`] for hashes and
//! targets, and the length-prefixed [`VarStr`] and [`VarBytes`]
//! families with their common aliases such as [`Str0_255`]. All types
//! hold `&'a [u8]`/`&'a str` references into a caller-owned buffer, so
//! nothing here allocates. Primitives are little-endian on the wire.

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

pub use crate::codec::{Decode, Encode, encode_to_slice, from_slice};
pub use crate::error::Error;
pub use crate::uint256::Uint256;
pub use crate::var_bytes::{
    Bytes0_16m, Bytes0_32, Bytes0_64k, Bytes0_255, Bytes1_32, Bytes1_64k, Bytes1_255, VarBytes,
};
pub use crate::var_str::{Str0_32, Str0_64k, Str0_255, Str1_32, Str1_255, VarStr};
