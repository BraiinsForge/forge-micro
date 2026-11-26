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

//! Tests for `PrefixRouter`.

extern crate std;

use core::cell::Cell;
use core::net::IpAddr;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embassy_futures::block_on;
use embedded_io_async::{Read, Write};

use super::PrefixRouter;
use crate::mock_socket::{MockSocket, PEER_IP};
use crate::request::{RequestParts, extract_request_parts};
use crate::response::write_status_only;
use crate::serve::{MAX_HEADERS, Service};
use crate::status::Status;

/// Parse area handed to `Connection::new`. Large enough for the request
/// lines below and nothing more; the listener sizes its own.
const PARSE_BUFFER_SIZE: usize = 512;

/// The prefix the firmware routes the REST API behind.
const API_PREFIX: &str = "/api/";

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
            Status::OK,
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
