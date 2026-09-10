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

//! Path-prefix routing across two [`Service`] handlers.
//!
//! Composing two handlers at the transport level is what lets one
//! listener serve two crates, so the router lives beside `Service`
//! itself and depends on neither handler crate.

use core::net::IpAddr;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};

use crate::request::RequestParts;
use crate::serve::Service;

#[cfg(test)]
#[path = "tests/router.rs"]
mod tests;

/// A [`Service`] that hands each request to one of two others, chosen by
/// the start of the request path.
///
/// The two route tables must not overlap: whichever service owns
/// `prefix` never sees a path outside it, and `fallback` never sees a
/// path inside it.
pub struct PrefixRouter<M, F> {
    /// The path prefix that selects `matched`.
    pub prefix: &'static str,
    /// Handles every request whose path starts with `prefix`.
    pub matched: M,
    /// Handles every other request.
    pub fallback: F,
}

impl<M, F> Service for PrefixRouter<M, F>
where
    M: Service,
    F: Service,
{
    async fn handle<TcpSocket, const N: usize>(
        &self,
        peer_ip: IpAddr,
        parts: &RequestParts,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit,
    {
        // The worker already extracted `parts` once, so routing reads
        // the path from it rather than borrowing `connection` again.
        if parts.path.starts_with(self.prefix) {
            self.matched.handle(peer_ip, parts, connection).await
        } else {
            self.fallback.handle(peer_ip, parts, connection).await
        }
    }
}
