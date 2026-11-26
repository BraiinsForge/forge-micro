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

//! The scrape service: the HTTP handler on [`Metrics::PORT`] that renders
//! the series map of the store as a Prometheus text exposition. It is
//! apart from the store so that the store holds state and ingestion only.
//!
//! [`Metrics::PORT`]: crate::consts::Metrics::PORT

use core::fmt::Write as _;
use core::net::IpAddr;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;

use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use ubos_http::{
    BufferWriter, RequestParts, Service, Status, format_content_length, start_response,
};
use ubos_logging::warn;

use crate::freshness::Expiry;
use crate::store::{MetricKey, MetricsStatics, StoredValue};

#[cfg(test)]
#[path = "tests/scrape.rs"]
mod tests;

/// Prometheus `/metrics` exposition handler.
pub struct PrometheusMetricsService<const MAX: usize, const BODY: usize> {
    statics: &'static MetricsStatics<MAX, BODY>,
    net: ubos_net::net::Handle,
}

impl<const MAX: usize, const BODY: usize> PrometheusMetricsService<MAX, BODY> {
    /// Serve the exposition out of `statics`, with the clock and the request
    /// count that `net` holds.
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
    let mut body_buffer = statics.body.lock().await;
    // The map lock is released before the socket write, so a slow scrape
    // never blocks ingestion.
    let body_len = {
        let map = statics.map.lock().await;
        build_prometheus_body(&mut body_buffer, &map, net)
    };

    let content_length = format_content_length(body_len);

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

    embedded_io_async::Write::write_all(connection, &body_buffer[..body_len]).await?;

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

/// Build the exposition body into `buffer` and return its length. A body
/// that does not fit warns, since a silent truncation ships a malformed
/// exposition the scraper rejects without a diagnostic here.
fn build_prometheus_body<const MAX: usize, const BODY: usize>(
    buffer: &mut [u8; BODY],
    map: &heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    net: ubos_net::net::Handle,
) -> usize {
    let mut writer = BufferWriter::new(buffer);

    // HELP and TYPE lines are optional, and the buffer is tight.
    let ntp_offset = net.ntp_epoch_offset();
    render_stored(&mut writer, map, ntp_offset, Instant::now().as_millis());

    let uptime_secs = Instant::now().as_secs();
    let ts_ms = if ntp_offset == 0 {
        None
    } else {
        Some((uptime_secs + ntp_offset) * 1000)
    };

    render_scalar(&mut writer, "uptime_seconds", uptime_secs, ts_ms);

    let request_count = net.request_count();
    render_scalar(&mut writer, "http_requests", request_count, ts_ms);

    let synced = u8::from(ntp_offset != 0);
    render_scalar(&mut writer, "ntp_synced", synced, ts_ms);

    if writer.overflowed() {
        warn!(
            "prometheus response buffer overflow; truncated exposition served \
             (raise the board's METRICS_RESPONSE_BYTES or reduce series count)"
        );
    }
    writer.len()
}

/// Render every stored series that has not expired at `now_ms`, in
/// monotonic milliseconds. The render is where a series whose publisher
/// stopped pushing leaves the exposition. The ingest task removes expired
/// entries only when the map is full.
fn render_stored<const MAX: usize>(
    writer: &mut BufferWriter<'_>,
    map: &heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    ntp_offset: u64,
    now_ms: u64,
) {
    for (key, stored) in map {
        if stored
            .expiry
            .is_some_and(|expiry| expiry.is_expired(now_ms))
        {
            continue;
        }
        render_metric(writer, key.0.as_str(), stored, ntp_offset);
    }
}

/// `ntp_offset` is what [`ubos_net::net::Handle::ntp_epoch_offset`] returns.
fn render_metric(writer: &mut BufferWriter<'_>, key: &str, stored: &StoredValue, ntp_offset: u64) {
    let _ = write!(writer, "{key} {}", stored.value);
    // Stored timestamps are monotonic ms since boot. Add the NTP epoch
    // offset to produce an absolute Unix-epoch timestamp. If NTP has
    // not synced yet, or the series is a state or a constant, omit the
    // timestamp so Prometheus stamps the sample at scrape time.
    let stamp_ms = stored.expiry.and_then(Expiry::stamp_ms);
    if let (Some(at_ms), true) = (stamp_ms, ntp_offset != 0) {
        let ts_ms = at_ms + ntp_offset * 1000;
        let _ = write!(writer, " {ts_ms}");
    }
    let _ = writeln!(writer);
}

fn render_scalar<T: core::fmt::Display>(
    writer: &mut BufferWriter<'_>,
    name: &str,
    value: T,
    ts_ms: Option<u64>,
) {
    let _ = write!(writer, "{name} {value}");
    if let Some(ts_ms) = ts_ms {
        let _ = write!(writer, " {ts_ms}");
    }
    let _ = writeln!(writer);
}
