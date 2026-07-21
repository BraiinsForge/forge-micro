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
    OutOfBounds {
        length: usize,
        min: usize,
        max: usize,
    },
    /// Bytes claimed to be UTF-8 were not valid.
    Utf8,
    /// A boolean byte was neither 0 nor 1.
    BadBool,
    /// An `Option` tag byte was neither 0 nor 1.
    BadOptionTag,
}
