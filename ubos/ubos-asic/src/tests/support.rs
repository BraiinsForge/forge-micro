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

//! Test-only helpers shared by the per-vendor driver tests.

use crate::transport::Transport;

/// Drive an immediately-ready future to completion with a no-op waker.
/// The mock transport never returns `Pending`, so a single poll resolves
/// it.
pub(crate) fn block_on<F: core::future::Future>(future: F) -> F::Output {
    use core::task::{Context, Poll, Waker};
    let mut context = Context::from_waker(Waker::noop());
    let mut future = core::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

/// Byte-level [`Transport`] recording every written byte and replaying a
/// scripted read stream. `read` drains `pending` in chunks and returns
/// `0` once exhausted, so a bring-up read sees its reply then a quiet
/// window.
pub(crate) struct MockTransport {
    pub(crate) writes: heapless::Vec<u8, 4096>,
    pending: heapless::Vec<u8, 2048>,
    pos: usize,
    pub(crate) baud: u32,
    /// When set, `set_baud` refuses the switch, exercising the
    /// bring-up abort path.
    pub(crate) fail_baud: bool,
}

impl MockTransport {
    pub(crate) fn new() -> Self {
        Self {
            writes: heapless::Vec::new(),
            pending: heapless::Vec::new(),
            pos: 0,
            baud: 0,
            fail_baud: false,
        }
    }

    pub(crate) fn with_reads(bytes: &[u8]) -> Self {
        let mut transport = Self::new();
        transport
            .pending
            .extend_from_slice(bytes)
            .expect("scripted reads exceed mock pending capacity");
        transport
    }
}

impl Transport for MockTransport {
    async fn write_all(&mut self, bytes: &[u8]) {
        self.writes
            .extend_from_slice(bytes)
            .expect("writes exceed mock capacity");
    }

    async fn read(&mut self, buf: &mut [u8], _timeout_ms: u32) -> usize {
        let available = self.pending.len() - self.pos;
        if available == 0 {
            return 0;
        }
        let n = available.min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        n
    }

    fn set_baud(&mut self, hz: u32) -> Result<(), crate::transport::BaudRateError> {
        if self.fail_baud {
            return Err(crate::transport::BaudRateError);
        }
        self.baud = hz;
        Ok(())
    }

    async fn delay_ms(&mut self, _ms: u32) {}
}
