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

//! A socket that replays one request out of a byte slice and collects the
//! response, so a real edge-http `Connection` can be built over it.

extern crate std;

use core::net::{IpAddr, Ipv4Addr};

use edge_nal::{Readable, TcpSplit};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

/// Peer address handed to the service under test.
pub(crate) const PEER_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// The error type the socket traits require of the mock.
#[derive(Debug)]
pub(crate) struct MockError;

impl core::fmt::Display for MockError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("mock socket error")
    }
}

impl core::error::Error for MockError {}

impl embedded_io_async::Error for MockError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// One request replayed out of a slice, with the response collected.
pub(crate) struct MockSocket<'req> {
    /// The part of the request not yet read.
    incoming: &'req [u8],
    /// Everything the handler has written back.
    outgoing: std::vec::Vec<u8>,
}

impl<'req> MockSocket<'req> {
    pub(crate) fn new(request: &'req [u8]) -> Self {
        Self {
            incoming: request,
            outgoing: std::vec::Vec::new(),
        }
    }
}

/// The read half `TcpSplit` hands out.
pub(crate) struct MockRead<'sock, 'req> {
    incoming: &'sock mut &'req [u8],
}

/// The write half `TcpSplit` hands out.
pub(crate) struct MockWrite<'sock> {
    outgoing: &'sock mut std::vec::Vec<u8>,
}

impl ErrorType for MockSocket<'_> {
    type Error = MockError;
}

impl ErrorType for MockRead<'_, '_> {
    type Error = MockError;
}

impl ErrorType for MockWrite<'_> {
    type Error = MockError;
}

/// Copy the front of `source` into `buffer` and report how much moved.
///
/// A short read is legal, and the empty slice reads as end of stream,
/// which is how the request ends.
fn drain(source: &mut &[u8], buffer: &mut [u8]) -> usize {
    let count = buffer.len().min(source.len());
    buffer[..count].copy_from_slice(&source[..count]);
    *source = &source[count..];
    count
}

impl Read for MockSocket<'_> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(drain(&mut self.incoming, buffer))
    }
}

impl Read for MockRead<'_, '_> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(drain(self.incoming, buffer))
    }
}

impl Readable for MockRead<'_, '_> {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Write for MockSocket<'_> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.outgoing.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Write for MockWrite<'_> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.outgoing.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'req> TcpSplit for MockSocket<'req> {
    type Read<'sock>
        = MockRead<'sock, 'req>
    where
        Self: 'sock;
    type Write<'sock>
        = MockWrite<'sock>
    where
        Self: 'sock;

    fn split(&mut self) -> (Self::Read<'_>, Self::Write<'_>) {
        let Self { incoming, outgoing } = self;
        (MockRead { incoming }, MockWrite { outgoing })
    }
}
