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

//! A write-position-tracking wrapper around a mutable byte slice.
//!
//! `BufferWriter` is used by all HTTP response builders to assemble
//! headers and body into a single flat buffer without heap allocation.
//! Overflow is silently discarded: writes past the buffer end return
//! `fmt::Error` and callers use `let _ = write!(...)` so a too-small
//! buffer produces a truncated but structurally valid response rather
//! than a panic. The `overflowed` flag records when this has happened
//! so callers that care (for example the prometheus exposition path)
//! can emit a log entry and avoid serving a silently malformed body.

use core::fmt::{self, Write};

/// Wrapper around a byte slice that tracks the write position.
///
/// Implements `core::fmt::Write` so standard `write!` macros work
/// directly. The `write_bytes` method handles raw binary payloads such
/// as gzip-compressed assets that are not valid UTF-8 text.
pub struct BufferWriter<'buffer> {
    buffer: &'buffer mut [u8],
    position: usize,
    overflowed: bool,
}

impl<'buffer> BufferWriter<'buffer> {
    /// Create a new writer positioned at the start of `buffer`.
    pub fn new(buffer: &'buffer mut [u8]) -> Self {
        Self {
            buffer,
            position: 0,
            overflowed: false,
        }
    }

    /// Return the number of bytes written so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.position
    }

    /// Return `true` if no bytes have been written yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.position == 0
    }

    /// Return `true` if any write since construction overflowed the
    /// buffer. Callers use this to decide whether to surface a log
    /// entry rather than ship a silently-truncated response.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Append raw bytes to the buffer.
    ///
    /// Used by static-asset routes such as the logo where the payload
    /// is not valid UTF-8 from the writer's point of view but still
    /// needs the same overflow handling as text writes.
    ///
    /// # Errors
    ///
    /// Returns `fmt::Error` when `bytes` would not fit in the remaining
    /// buffer space. The `overflowed` flag is latched to `true` so the
    /// condition is still visible after the caller ignored the error.
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
