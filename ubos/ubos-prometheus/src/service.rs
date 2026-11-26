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

//! The handler that answers a scrape with the exposition the store renders.

use core::net::IpAddr;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};
use ubos_http::{RequestParts, Service, Status, format_content_length, start_response};
use ubos_metrics::metrics::MetricsStatics;

/// Prometheus `/metrics` exposition handler.
pub struct PrometheusMetricsService<const MAX: usize, const BODY: usize> {
    statics: &'static MetricsStatics<MAX, BODY>,
    net: ubos_net::net::Handle,
}

impl<const MAX: usize, const BODY: usize> PrometheusMetricsService<MAX, BODY> {
    /// Serve the exposition out of `statics`, with the clock offset that
    /// `net` holds.
    #[must_use]
    pub const fn new(
        statics: &'static MetricsStatics<MAX, BODY>,
        net: ubos_net::net::Handle,
    ) -> Self {
        Self { statics, net }
    }
}

impl<const MAX: usize, const BODY: usize> Service for PrometheusMetricsService<MAX, BODY> {
    async fn handle<TcpSocket, const N: usize>(
        &self,
        _peer_ip: IpAddr,
        parts: &RequestParts,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit,
    {
        // A path over the cap arrives empty and falls through the match.
        let is_get = parts.method.as_str() == "GET";

        match (is_get, parts.path.as_str()) {
            (true, "/metrics") => serve_metrics(self.statics, self.net, connection).await,
            (false, "/metrics") => serve_method_not_allowed(connection).await,
            _ => serve_not_found(connection).await,
        }
    }
}

async fn serve_metrics<T, const N: usize, const MAX: usize, const BODY: usize>(
    statics: &'static MetricsStatics<MAX, BODY>,
    net: ubos_net::net::Handle,
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let exposition = statics.render(net.ntp_epoch_offset()).await;
    let body = exposition.as_bytes();

    let content_length = format_content_length(body.len());

    start_response(
        connection,
        Status::OK,
        // The dashboard on port 80 reads this endpoint cross-origin
        // for the state snapshot download. The wildcard is safe on an
        // unauthenticated listener and keeps the handler free of
        // Origin parsing; it also forbids credentialed fetches.
        &[
            ("Content-Type", "text/plain; version=0.0.4"),
            ("Content-Length", content_length.as_str()),
            ("Connection", "close"),
            ("Access-Control-Allow-Origin", "*"),
        ],
    )
    .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Error responses carry the wildcard CORS header too, so a cross-origin
/// caller sees the real status instead of an opaque CORS failure.
async fn serve_not_found<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ubos_http::write_status_only(
        connection,
        Status::NOT_FOUND,
        "text/plain",
        &[
            ("Connection", "close"),
            ("Access-Control-Allow-Origin", "*"),
        ],
        b"",
    )
    .await
}

/// Includes the `Allow` header RFC 7231 requires on a 405.
async fn serve_method_not_allowed<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ubos_http::write_status_only(
        connection,
        Status::METHOD_NOT_ALLOWED,
        "text/plain",
        &[
            ("Allow", "GET"),
            ("Connection", "close"),
            ("Access-Control-Allow-Origin", "*"),
        ],
        b"",
    )
    .await
}
