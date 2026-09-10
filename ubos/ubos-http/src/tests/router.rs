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

//! Tests for `PrefixRouter` — the split that puts two crates on one port.
//!
//! Routing is the only part of the merged listener that runs on the host,
//! and a prefix that selects the wrong service sends every REST call to
//! the dashboard's 404 or every page to the API's. The mock socket below
//! replays one request out of a byte slice so a real edge-http
//! `Connection` can be built over it.

extern crate std;

use core::cell::Cell;
use core::net::{IpAddr, Ipv4Addr};

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::{Readable, TcpSplit};
use embassy_futures::block_on;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

use super::PrefixRouter;
use crate::request::{RequestParts, extract_request_parts};
use crate::response::write_status_only;
use crate::serve::{MAX_HEADERS, Service};

/// Parse area handed to `Connection::new`. Large enough for the request
/// lines below and nothing more; the listener sizes its own.
const PARSE_BUFFER_SIZE: usize = 512;

/// The prefix the firmware routes the REST API behind.
const API_PREFIX: &str = "/api/";

/// Peer address reported to the handler. No route under test reads it.
const PEER_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Failure of the mock socket. It never fails, so the type is empty and
/// the impls below exist only to satisfy the `ErrorType` bound.
#[derive(Debug)]
struct MockError;

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
struct MockSocket<'req> {
    /// The part of the request not yet read.
    incoming: &'req [u8],
    /// Everything the handler has written back.
    outgoing: std::vec::Vec<u8>,
}

impl<'req> MockSocket<'req> {
    fn new(request: &'req [u8]) -> Self {
        Self {
            incoming: request,
            outgoing: std::vec::Vec::new(),
        }
    }
}

/// The read half `TcpSplit` hands out.
struct MockRead<'sock, 'req> {
    incoming: &'sock mut &'req [u8],
}

/// The write half `TcpSplit` hands out.
struct MockWrite<'sock> {
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

/// A service that records that it ran and answers with its own name.
struct Recorder {
    name: &'static str,
    called: Cell<bool>,
}

impl Recorder {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            called: Cell::new(false),
        }
    }
}

impl Service for Recorder {
    async fn handle<TcpSocket, const N: usize>(
        &self,
        _peer_ip: IpAddr,
        _parts: &RequestParts,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit,
    {
        self.called.set(true);
        write_status_only(
            connection,
            200,
            "OK",
            "text/plain",
            &[],
            self.name.as_bytes(),
        )
        .await
    }
}

/// Route `request` through a router over two recorders and report which
/// of the two ran, as `(matched, fallback)`.
fn route(request: &[u8]) -> (bool, bool) {
    let router = PrefixRouter {
        prefix: API_PREFIX,
        matched: Recorder::new("api"),
        fallback: Recorder::new("webui"),
    };
    let mut socket = MockSocket::new(request);
    let mut parse_buffer = [0u8; PARSE_BUFFER_SIZE];

    block_on(async {
        let mut connection = Connection::<_, MAX_HEADERS>::new(&mut parse_buffer, &mut socket)
            .await
            .expect("the mock request parses");
        let parts = extract_request_parts(&mut connection).expect("the parts extract");
        router
            .handle(PEER_IP, &parts, &mut connection)
            .await
            .expect("the recorder answers");
    });

    (router.matched.called.get(), router.fallback.called.get())
}

#[test]
fn a_path_under_the_prefix_reaches_the_matched_service() {
    assert_eq!(
        route(b"GET /api/info HTTP/1.1\r\nHost: board\r\n\r\n"),
        (true, false)
    );
}

#[test]
fn the_root_path_reaches_the_fallback_service() {
    assert_eq!(
        route(b"GET / HTTP/1.1\r\nHost: board\r\n\r\n"),
        (false, true)
    );
}

#[test]
fn a_path_beside_the_prefix_reaches_the_fallback_service() {
    assert_eq!(
        route(b"GET /logo.svg HTTP/1.1\r\nHost: board\r\n\r\n"),
        (false, true)
    );
}

#[test]
fn the_prefix_without_its_trailing_slash_reaches_the_fallback_service() {
    assert_eq!(
        route(b"GET /api HTTP/1.1\r\nHost: board\r\n\r\n"),
        (false, true)
    );
}

#[test]
fn the_method_does_not_change_the_route() {
    assert_eq!(
        route(b"POST /api/login HTTP/1.1\r\nHost: board\r\nContent-Length: 0\r\n\r\n"),
        (true, false)
    );
}
