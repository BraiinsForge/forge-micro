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

use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use ubos_logging::warn;

use crate::consts::Metrics;
use crate::store::{METRICS, MetricKey, StoredValue};
use ubos_http::{
    BufferWriter, RequestParts, Service, Status, format_content_length, start_response,
};

pub use crate::store::{METRIC_LINE_BYTES, MetricsStatics, run_ingest};

/// A metric value.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum Value {
    /// Signed integer value.
    Int(i64),
    /// Unsigned integer value.
    Uint(u64),
    /// Floating-point value.
    Float(f64),
}

/// A label value, either a static string or an owned inline string for
/// dynamic values such as chip identifiers.
#[non_exhaustive]
pub enum LabelValue {
    /// A compile-time constant string.
    Static(&'static str),
    /// An owned string for values known only at runtime.
    Short(heapless::String<24>),
}

impl LabelValue {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            LabelValue::Static(text) => text,
            LabelValue::Short(text) => text.as_str(),
        }
    }
}

/// A name-value label pair attached to a metric.
pub struct Label {
    pub name: &'static str,
    pub value: LabelValue,
}

/// Fixed-capacity label vector carried by a [`Metric`]. Re-exported as a
/// type alias so callers building labels for `Metric::with_labels` need
/// not name the const generic capacity directly.
pub type Labels = heapless::Vec<Label, { Metrics::MAX_LABELS }>;

/// A single metric observation to push into the exposition channel.
///
/// Fields are private; construct through [`Metric::with_no_labels`],
/// [`Metric::with_labels`] or [`Metric::indexed`], each of which stamps
/// the metric with the current monotonic time, and push it with
/// [`Metric::push`] or [`Metric::try_push`].
///
/// The stored timestamp is **monotonic milliseconds since boot**
/// (`embassy_time::Instant::now().as_millis()`). The exposition path
/// adds the NTP epoch offset at scrape time to render an absolute
/// Unix-epoch timestamp.
pub struct Metric {
    name: &'static str,
    labels: Labels,
    value: Value,
    timestamp_ms: u64,
}

impl Metric {
    /// Build a metric with no labels, stamped with the current
    /// monotonic time.
    #[must_use]
    pub fn with_no_labels(name: &'static str, value: Value) -> Self {
        Self {
            name,
            labels: Labels::new(),
            value,
            timestamp_ms: Instant::now().as_millis(),
        }
    }

    /// Build a metric carrying the provided labels, stamped with the
    /// current monotonic time.
    #[must_use]
    pub fn with_labels(name: &'static str, labels: Labels, value: Value) -> Self {
        Self {
            name,
            labels,
            value,
            timestamp_ms: Instant::now().as_millis(),
        }
    }

    /// Build a metric carrying one `<label>="<index>"` tag per entry of
    /// `indices`, in the order given, stamped with the current monotonic
    /// time.
    ///
    /// The shape every index-tagged series in this tree uses: a pool slot,
    /// a hashboard chain, or both on one series. Each index renders
    /// through [`LabelValue::Short`], whose capacity bounds the rendered
    /// text. An index cannot approach that bound. Entries beyond the label
    /// capacity are dropped, as [`Labels`] drops them everywhere.
    #[must_use]
    pub fn indexed(name: &'static str, indices: &[(&'static str, usize)], value: Value) -> Self {
        let mut labels = Labels::new();
        for &(label, index) in indices {
            let mut index_str = heapless::String::new();
            let _ = write!(index_str, "{index}");
            let _ = labels.push(Label {
                name: label,
                value: LabelValue::Short(index_str),
            });
        }
        Self::with_labels(name, labels, value)
    }

    /// Push the metric into the exposition channel, waiting while the
    /// channel is full.
    pub async fn push(self) {
        METRICS.send(self).await;
    }

    /// Push the metric into the exposition channel, or drop it when the
    /// channel is full.
    ///
    /// The path for a caller that must stay able to make progress while
    /// the channel stands still. [`run_ingest`] is the only reader, so a
    /// caller on a core that cannot run it has no way to tell how long a
    /// [`Metric::push`] would wait. Every series pushed this way is pushed
    /// on a ticker, so a dropped sample is replaced by the next one.
    pub fn try_push(self) {
        let _ = METRICS.try_send(self);
    }

    /// The metric's identifier as emitted in the Prometheus body.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The metric's current value.
    #[must_use]
    pub fn value(&self) -> Value {
        self.value
    }

    /// Monotonic-ms timestamp captured when the metric was constructed.
    #[must_use]
    pub fn timestamp(&self) -> u64 {
        self.timestamp_ms
    }

    /// Labels attached to this metric.
    #[must_use]
    pub fn labels(&self) -> &[Label] {
        &self.labels
    }
}

/// Prometheus `/metrics` exposition handler.
///
/// One [`Service`] shared across all exposition workers. It reads the
/// map [`run_ingest`] keeps current and renders into the render buffer
/// beside it, both owned by the board's [`MetricsStatics`].
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
        // The worker already extracted `parts`, so no second copy of the
        // method and path is needed here. A path over the 128-byte cap
        // arrives empty, which falls through the match below exactly as
        // the oversize path used to once it was replaced with `/`: neither
        // ever equals `/metrics`.
        let is_get = parts.method.as_str() == "GET";

        match (is_get, parts.path.as_str()) {
            (true, "/metrics") => serve_metrics(self.statics, self.net, connection).await,
            (false, "/metrics") => serve_method_not_allowed(connection).await,
            _ => serve_not_found(connection).await,
        }
    }
}

/// Build the Prometheus body and send it with an accurate `Content-Length`.
async fn serve_metrics<T, const N: usize, const MAX: usize, const BODY: usize>(
    statics: &'static MetricsStatics<MAX, BODY>,
    net: ubos_net::net::Handle,
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut body_buffer = statics.body.lock().await;
    // Render under the map lock, then release it before the socket write
    // so a slow scrape blocks only other scrapes, never ingestion.
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

/// Write the 404 response.
///
/// Error responses carry the same wildcard CORS header as the metrics
/// response so a cross-origin caller sees the real HTTP status instead
/// of an opaque CORS failure.
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
        // `write_status_only` no longer closes on the caller's behalf, so
        // this listener writes the header itself, as its success path
        // already does.
        &[
            ("Connection", "close"),
            ("Access-Control-Allow-Origin", "*"),
        ],
        b"",
    )
    .await
}

/// Write the 405 response. Includes `Allow: GET` per RFC 7231 §6.5.5.
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

/// Build the Prometheus text exposition body into `buffer`. Returns byte count.
///
/// Emits a single `defmt::warn!` if the response did not fit in the
/// buffer: a silent truncation would ship a malformed exposition that
/// the scraper rejects with no diagnostic on this side. The metric
/// budget is sized for the current series count plus headroom; if this
/// warning ever fires, raise the board's `METRICS_RESPONSE_BYTES` or
/// trim the metric set.
fn build_prometheus_body<const MAX: usize, const BODY: usize>(
    buffer: &mut [u8; BODY],
    map: &heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    net: ubos_net::net::Handle,
) -> usize {
    let mut writer = BufferWriter::new(buffer);

    // HELP/TYPE lines are omitted throughout — they are optional in the
    // Prometheus text format and the buffer is tight.
    let ntp_offset = net.ntp_epoch_offset();
    for (key, stored) in map {
        render_metric(&mut writer, key.0.as_str(), stored, ntp_offset);
    }

    // Render hardcoded derived metrics.
    let uptime_secs = Instant::now().as_secs();
    let ts_ms = if ntp_offset == 0 {
        None
    } else {
        Some((uptime_secs + ntp_offset) * 1000)
    };

    render_scalar(&mut writer, "board_uptime_seconds", uptime_secs, ts_ms);

    let request_count = net.request_count();
    render_scalar(
        &mut writer,
        "board_http_requests_total",
        request_count,
        ts_ms,
    );

    let synced = u8::from(ntp_offset != 0);
    render_scalar(&mut writer, "board_ntp_synced", synced, ts_ms);

    if writer.overflowed() {
        warn!(
            "prometheus response buffer overflow; truncated exposition served \
             (raise the board's METRICS_RESPONSE_BYTES or reduce series count)"
        );
    }
    writer.len()
}

/// Render one stored metric line. The key string includes name and labels.
/// `ntp_offset` is the seconds from the monotonic clock to Unix time, or 0
/// before the first sync.
fn render_metric(writer: &mut BufferWriter<'_>, key: &str, stored: &StoredValue, ntp_offset: u64) {
    // The key encodes labels as `name{label=value,...}` or just `name`.
    let _ = write!(writer, "{key} ");
    match &stored.value {
        Value::Int(value) => {
            let _ = write!(writer, "{value}");
        }
        Value::Uint(value) => {
            let _ = write!(writer, "{value}");
        }
        Value::Float(value) => render_float_value(writer, *value),
    }
    // Stored timestamps are monotonic ms since boot. Add the NTP epoch
    // offset to produce an absolute Unix-epoch timestamp. If NTP has
    // not synced yet, omit the timestamp so Prometheus stamps the
    // sample at scrape time.
    if ntp_offset != 0 {
        let ts_ms = stored.timestamp_ms + ntp_offset * 1000;
        let _ = write!(writer, " {ts_ms}");
    }
    let _ = writeln!(writer);
}

/// Render a scalar (integer-like) derived metric line.
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

/// Render a float value, handling NaN and infinity per Prometheus spec.
fn render_float_value(writer: &mut BufferWriter<'_>, value: f64) {
    if value.is_nan() {
        let _ = write!(writer, "NaN");
    } else if value.is_infinite() {
        if value > 0.0 {
            let _ = write!(writer, "+Inf");
        } else {
            let _ = write!(writer, "-Inf");
        }
    } else {
        let _ = write!(writer, "{value:.6}");
    }
}
