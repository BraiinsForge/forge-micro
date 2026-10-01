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

//! A `core::fmt::Write` over a byte slice. A write past the end returns
//! `fmt::Error` and latches `overflowed`, so a caller that ignores the
//! error can still refuse to ship a truncated body.

use core::fmt::{self, Write};

pub struct BufferWriter<'buffer> {
    buffer: &'buffer mut [u8],
    position: usize,
    overflowed: bool,
}

impl<'buffer> BufferWriter<'buffer> {
    pub fn new(buffer: &'buffer mut [u8]) -> Self {
        Self {
            buffer,
            position: 0,
            overflowed: false,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.position
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.position == 0
    }

    /// Any write since construction overflowed the buffer.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Append raw bytes, for payloads that are not UTF-8.
    ///
    /// # Errors
    ///
    /// `fmt::Error` when `bytes` does not fit; `overflowed` latches.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> fmt::Result {
        let remaining = self.buffer.len() - self.position;
        if bytes.len() > remaining {
            self.overflowed = true;
            return Err(fmt::Error);
        }
        self.buffer[self.position..self.position + bytes.len()].copy_from_slice(bytes);
        self.position += bytes.len();
        Ok(())
    }
}

impl Write for BufferWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_bytes(text.as_bytes())
    }
}
