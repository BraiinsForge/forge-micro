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

//! Newline-delimited framing buffer for stream reads.

use crate::error::ParseError;

#[cfg(test)]
#[path = "tests/line_buffer.rs"]
mod tests;

pub struct LineBuffer<const N: usize> {
    buf: [u8; N],
    used: usize,
}

impl<const N: usize> LineBuffer<N> {
    #[must_use]
    pub const fn new() -> Self {
        LineBuffer {
            buf: [0u8; N],
            used: 0,
        }
    }

    // Returns the unused portion of the internal buffer for the transport to fill.
    pub fn writable(&mut self) -> &mut [u8] {
        &mut self.buf[self.used..]
    }

    // Marks n bytes written into the buffer by a previous writable() fill.
    pub fn advance(&mut self, n: usize) {
        debug_assert!(self.used + n <= N);
        self.used += n;
    }

    // Returns the next complete line without its trailing '\n', compacting
    // the buffer after the borrow is dropped.
    pub fn next_line(&mut self) -> Result<Option<&[u8]>, ParseError<'static>> {
        if let Some(position) = self.buf[..self.used].iter().position(|&byte| byte == b'\n') {
            return Ok(Some(&self.buf[..position]));
        }
        if self.used == N {
            return Err(ParseError::LineTooLong);
        }
        Ok(None)
    }

    // Consumes the most recently returned line and compacts the buffer.
    // Call this after processing the slice returned by next_line().
    pub fn consume(&mut self) {
        if let Some(position) = self.buf[..self.used].iter().position(|&byte| byte == b'\n') {
            let remaining = self.used - (position + 1);
            self.buf.copy_within(position + 1..self.used, 0);
            self.used = remaining;
        }
    }
}

impl<const N: usize> Default for LineBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}
