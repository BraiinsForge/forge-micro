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

//! Tests for `serve_one`.

use core::net::IpAddr;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embassy_futures::block_on;
use embedded_io_async::{Read, Write};

use super::{BAD_FRAMING_RESPONSE, Ended, Service, serve_one};
use crate::consts::Http;
use crate::mock_socket::{MockSocket, PEER_IP};
use crate::request::RequestParts;

/// A handler for requests that must be refused before one runs.
struct Unreachable;

impl Service for Unreachable {
    async fn handle<TcpSocket, const N: usize>(
        &self,
        _peer_ip: IpAddr,
        _parts: &RequestParts,
        _connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit,
    {
        unreachable!("BUG: a refused request reached the handler")
    }
}

#[test]
fn a_content_length_that_is_not_a_number_is_refused_with_a_400() {
    let mut socket =
        MockSocket::new(b"GET /login HTTP/1.1\r\nHost: board\r\nContent-Length: abc\r\n\r\n");
    let mut parse_buffer = [0u8; Http::REQUEST_BUFFER_SIZE];

    let ended = block_on(serve_one(
        &mut parse_buffer,
        &mut socket,
        &Unreachable,
        PEER_IP,
        Http::IDLE_TIMEOUT,
    ));

    assert!(matches!(ended, Err(Ended::Refuse(response)) if response == BAD_FRAMING_RESPONSE));
}
