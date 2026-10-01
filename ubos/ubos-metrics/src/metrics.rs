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

//! Prometheus metrics exposition server running on [`Metrics::PORT`].
//!
//! Endpoint model: `../../../docs/METRICS.md`.

use core::fmt::Write as _;
use core::net::IpAddr;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;

use embassy_time::{Duration, Instant};
use embedded_io_async::{Read, Write};
use ubos_logging::warn;

use crate::consts::Metrics;
use crate::freshness::Expiry;
use crate::name::{Name, Unit};
use crate::store::{METRICS, MetricKey, StoredValue};
use crate::value::Value;
use ubos_http::{
    BufferWriter, RequestParts, Service, Status, format_content_length, start_response,
};

#[cfg(test)]
#[path = "tests/metrics.rs"]
mod tests;

pub use crate::store::{METRIC_LINE_BYTES, MetricsStatics, run_ingest};

/// Bytes the text of one label value may take. The longest value a push
/// site writes is a short word or an index, and each byte costs one byte
/// for every label of every queued metric.
const LABEL_VALUE_CAPACITY: usize = 24;

pub(crate) struct Label {
    pub(crate) name: &'static str,
    pub(crate) value: heapless::String<LABEL_VALUE_CAPACITY>,
}

/// Fixed-capacity label vector carried by a [`Metric`].
type Labels = heapless::Vec<Label, { Metrics::MAX_LABELS }>;

/// A single metric observation to push into the exposition channel.
///
/// Fields are private; construct through the `metric!` macro, which
/// checks the name and stamps the metric with the current monotonic
/// time, add each label with [`Metric::label`], and push it with
/// [`Metric::push`] or [`Metric::try_push`].
///
/// A measured metric stores the time of its measurement in **monotonic
/// milliseconds since boot** (`embassy_time::Instant::now().as_millis()`)
/// and a TTL. The exposition path adds the NTP epoch offset at scrape
/// time to render an absolute Unix-epoch timestamp. A constant stores
/// neither and renders with no timestamp.
pub struct Metric {
    name: &'static str,
    labels: Labels,
    value: Value,
    expiry: Option<Expiry>,
}

impl Metric {
    /// Build a metric with no labels under a checked name, stamped with
    /// the current monotonic time.
    ///
    /// Push sites reach it through the `metric!` macro, which builds the
    /// name in a `const` context so that the check runs in the build.
    #[must_use]
    pub fn new<U: Unit>(name: Name<U>, value: U) -> Self {
        Self::stamped(name.as_str(), value.into())
    }

    /// Add the label `name` with the text of `value`.
    ///
    /// The store sorts the labels of a series, so the order of the calls
    /// does not change the series. Only the label capacity makes it
    /// matter, because the labels beyond it are dropped, the last first.
    /// A value that does not fit the capacity of a label value is not
    /// stored: the label keeps the pieces of the text that fitted, and a
    /// single text that is too long leaves it empty.
    #[must_use]
    pub fn label(mut self, name: &'static str, value: impl core::fmt::Display) -> Self {
        self.push_label(name, &value);
        self
    }

    /// Write `value` into a new label `name`. The value is a trait object
    /// so that the code is compiled once and not once for each type a
    /// push site labels with.
    fn push_label(&mut self, name: &'static str, value: &dyn core::fmt::Display) {
        let mut text = heapless::String::new();
        let _ = write!(text, "{value}");
        let _ = self.labels.push(Label { name, value: text });
    }

    /// Build a metric from a value that is already converted, stamped with
    /// the current monotonic time. The public constructor ends here, so
    /// the code is compiled once and not once for each unit pushed.
    fn stamped(name: &'static str, value: Value) -> Self {
        Self {
            name,
            labels: Labels::new(),
            value,
            expiry: Some(Expiry::new(
                Instant::now().as_millis(),
                Metrics::DEFAULT_TTL,
            )),
        }
    }

    /// Make the metric a measurement that expires `ttl` after now, rounded
    /// up to whole seconds. A publisher states its push interval times
    /// [`Metrics::TTL_INTERVALS`], so a series that stops leaves the
    /// exposition. The last of [`Metric::ttl`] and [`Metric::constant`]
    /// wins.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.expiry = Some(Expiry::new(Instant::now().as_millis(), ttl));
        self
    }

    /// Make the metric a constant: it never expires and renders with no
    /// timestamp. Push it with [`Metric::push`], so it is never lost. The
    /// last of [`Metric::ttl`] and [`Metric::constant`] wins.
    #[must_use]
    pub fn constant(mut self) -> Self {
        self.expiry = None;
        self
    }

    /// Awaits while the channel is full.
    pub async fn push(self) {
        METRICS.send(self).await;
    }

    /// Drops the metric when the channel is full, for a caller that must
    /// keep making progress while [`run_ingest`] cannot run. Every series
    /// pushed this way is pushed on a ticker, so the next sample replaces
    /// a dropped one.
    pub fn try_push(self) {
        let _ = METRICS.try_send(self);
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn value(&self) -> Value {
        self.value
    }

    /// When the metric was measured and how long it stays valid, or
    /// `None` for a constant.
    pub(crate) fn expiry(&self) -> Option<Expiry> {
        self.expiry
    }

    #[must_use]
    pub(crate) fn labels(&self) -> &[Label] {
        &self.labels
    }
}

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
    // not synced yet, or the series is a constant, omit the timestamp so
    // Prometheus stamps the sample at scrape time.
    if let (Some(expiry), true) = (stored.expiry, ntp_offset != 0) {
        let ts_ms = expiry.at_ms() + ntp_offset * 1000;
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
