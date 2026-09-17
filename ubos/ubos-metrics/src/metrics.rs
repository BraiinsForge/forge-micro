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

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use ubos_logging::warn;

use crate::consts::Metrics;
use ubos_http::{BufferWriter, RequestParts, Service, format_content_length};

#[cfg(test)]
#[path = "tests/metrics.rs"]
mod tests;

/// A metric value.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum Value {
    /// Signed integer value.
    Int(i64),
    /// Unsigned integer value.
    Uint(u64),
    /// Floating-point value.
    Float(f32),
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
    fn as_str(&self) -> &str {
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
/// Fields are private; construct through [`Metric::with_no_labels`] or
/// [`Metric::with_labels`]. Both constructors stamp the metric with the
/// current monotonic time.
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

/// Stored value associated with a map key.
struct StoredValue {
    value: Value,
    timestamp_ms: u64,
}

/// Canonical map key: metric name plus labels sorted by name and serialized.
///
/// Labels are sorted so that `{a="1",b="2"}` and `{b="2",a="1"}` map to
/// the same key.
///
/// Budget: the longest key any metric renders is
/// [`LONGEST_RENDERED_KEY`], and the capacity is checked against it
/// below rather than against a length restated here. A prose budget is
/// what went stale last time: it named a 49-byte key while a longer one
/// was already being emitted, and the two-label per-chain series that
/// followed overran the cap and was dropped at runtime for a `warn!`.
///
/// Each byte costs [`Metrics::MAX_METRICS`] bytes of static map, so this
/// is raised deliberately rather than rounded up. A metric longer than
/// the constant below must move both, and the test beside it is what
/// catches one that does not.
const KEY_CAPACITY: usize = 80;

/// The longest key the tree renders, in the label-expanded form
/// [`MetricKey::from_metric`] builds: `hashboard_chain_chip_temperature_millicelsius`
/// with the `chain` and `stat` labels `ubos-thermal` gives it. A chain
/// index is one digit for every `MAX_CHAINS` below ten.
const LONGEST_RENDERED_KEY: &str =
    "hashboard_chain_chip_temperature_millicelsius{chain=\"0\",stat=\"highest\"}";

const _: () = assert!(
    LONGEST_RENDERED_KEY.len() <= KEY_CAPACITY,
    "KEY_CAPACITY is below the longest key a metric renders"
);

#[derive(PartialEq, Eq, Hash)]
struct MetricKey(heapless::String<KEY_CAPACITY>);

impl MetricKey {
    /// Build a key from a metric, sorting its labels by name.
    fn from_metric(metric: &Metric) -> Option<Self> {
        let labels = metric.labels();
        let mut sorted_indices: [usize; Metrics::MAX_LABELS] = [0; Metrics::MAX_LABELS];
        let count = labels.len();
        for (index, slot) in sorted_indices.iter_mut().enumerate().take(count) {
            *slot = index;
        }
        // Insertion sort is fine for the bounded label count.
        for outer in 1..count {
            let pivot = sorted_indices[outer];
            let mut inner = outer;
            while inner > 0 && labels[sorted_indices[inner - 1]].name > labels[pivot].name {
                sorted_indices[inner] = sorted_indices[inner - 1];
                inner -= 1;
            }
            sorted_indices[inner] = pivot;
        }

        let mut key = heapless::String::<KEY_CAPACITY>::new();
        key.push_str(metric.name()).ok()?;
        if count > 0 {
            key.push('{').ok()?;
            for (position, &slot) in sorted_indices[..count].iter().enumerate() {
                if position > 0 {
                    key.push(',').ok()?;
                }
                key.push_str(labels[slot].name).ok()?;
                key.push_str("=\"").ok()?;
                push_escaped_label_value(&mut key, labels[slot].value.as_str())?;
                key.push('"').ok()?;
            }
            key.push('}').ok()?;
        }
        Some(MetricKey(key))
    }
}

/// Append a Prometheus-escaped label value to `key`. Returns `None` if the
/// buffer fills.
fn push_escaped_label_value(key: &mut heapless::String<KEY_CAPACITY>, value: &str) -> Option<()> {
    for character in value.chars() {
        match character {
            '\\' => key.push_str("\\\\").ok()?,
            '"' => key.push_str("\\\"").ok()?,
            '\n' => key.push_str("\\n").ok()?,
            other => key.push(other).ok()?,
        }
    }
    Some(())
}

/// Shared channel through which any task can push metrics.
static METRICS: Channel<CriticalSectionRawMutex, Metric, { Metrics::CHANNEL_DEPTH }> =
    Channel::new();

/// Current metric values, shared between [`run_ingest`] (the sole
/// writer) and the exposition workers (readers).
///
/// Hoisted into a `static` so it can be placed in CCM RAM on boards that
/// enable the `ccmram-bss` feature; embedding the map in a task future
/// would force it into `.bss`, which is what blew the stm32f4 main SRAM
/// during the api refactor.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
static METRIC_MAP: Mutex<
    CriticalSectionRawMutex,
    heapless::index_map::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
> = Mutex::new(heapless::index_map::FnvIndexMap::new());

/// Hoisted exposition body buffer. Lives next to `METRIC_MAP` so the
/// 2 KiB scratch does not ride on a worker's async frame.
///
/// A single `Mutex` shared across all exposition workers: with a pool
/// depth above one, concurrent scrapes serialize on this lock while
/// rendering. `serve_metrics` drops the `METRIC_MAP` guard before the
/// socket write, so a slow scrape blocks only other scrapes, never the
/// ingestion task.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
static METRICS_BODY_BUFFER: Mutex<CriticalSectionRawMutex, [u8; Metrics::RESPONSE_BUFFER_SIZE]> =
    Mutex::new([0; Metrics::RESPONSE_BUFFER_SIZE]);

/// Push a metric into the exposition channel. Awaits if the channel is full.
pub async fn push(metric: Metric) {
    METRICS.send(metric).await;
}

/// Push one metric carrying one `<label>="<index>"` tag per entry of
/// `indices`, in the order given.
///
/// The shape every index-tagged series in this tree uses: a pool slot, a
/// hashboard chain, or both on one series. Each index renders through
/// [`LabelValue::Short`], whose capacity bounds the rendered text. An
/// index cannot approach that bound. Entries beyond the label capacity
/// are dropped, as [`Labels`] drops them everywhere.
pub async fn push_indexed_metrics(
    name: &'static str,
    indices: &[(&'static str, usize)],
    value: Value,
) {
    let mut labels = Labels::new();
    for &(label, index) in indices {
        let mut index_str = heapless::String::new();
        let _ = write!(index_str, "{index}");
        let _ = labels.push(Label {
            name: label,
            value: LabelValue::Short(index_str),
        });
    }
    push(Metric::with_labels(name, labels, value)).await;
}

/// Push one metric carrying a single `<label>="<index>"` tag.
///
/// `stratum-client`, `ubos-mining`, and `ubos-thermal` each had their
/// own copy of this loop until it moved here.
pub async fn push_indexed_metric(
    name: &'static str,
    label: &'static str,
    index: usize,
    value: Value,
) {
    push_indexed_metrics(name, &[(label, index)], value).await;
}

/// Push one metric carrying a `pool="<slot>"` label.
///
/// The per-slot shape every pool-indexed series in this tree uses.
pub async fn push_slot_metric(name: &'static str, slot: usize, value: Value) {
    push_indexed_metric(name, "pool", slot, value).await;
}

/// Consume pushed metrics into the shared map.
///
/// The sole writer of [`struct@METRIC_MAP`]. Call this from a firmware
/// `#[embassy_executor::task]`; the exposition workers spawned via
/// `serve_http_pool!` with [`PrometheusMetricsService`] read the map this
/// task keeps current, so the exposition always reflects the latest
/// pushed values without a drain step on the serve path.
pub async fn run_ingest() -> ! {
    loop {
        let metric = METRICS.receive().await;
        let mut map = METRIC_MAP.lock().await;
        upsert_metric(&mut map, &metric);
    }
}

/// Task shell around [`run_ingest`]. The body is the same on every
/// board, so the macro takes nothing.
#[macro_export]
macro_rules! metrics_ingest_task {
    () => {
        #[embassy_executor::task]
        async fn metrics_ingest_task() -> ! {
            ubos_metrics::metrics::run_ingest().await
        }
    };
}

/// Upsert a metric into the map, sorting its labels into a canonical key.
fn upsert_metric(
    map: &mut heapless::index_map::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
    metric: &Metric,
) {
    let Some(key) = MetricKey::from_metric(metric) else {
        warn!(
            "metrics key encoding overflowed for {}; dropping metric (raise KEY_CAPACITY)",
            metric.name()
        );
        return;
    };
    let stored = StoredValue {
        value: metric.value(),
        timestamp_ms: metric.timestamp(),
    };
    if map.insert(key, stored).is_err() {
        warn!("metrics map full, dropping metric");
    }
}

/// Prometheus `/metrics` exposition handler.
///
/// A unit [`Service`] shared across all exposition workers. It reads the
/// shared [`struct@METRIC_MAP`] kept current by [`run_ingest`] and
/// renders into the shared [`struct@METRICS_BODY_BUFFER`].
pub struct PrometheusMetricsService;

impl Service for PrometheusMetricsService {
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
            (true, "/metrics") => serve_metrics(connection).await,
            (false, "/metrics") => serve_method_not_allowed(connection).await,
            _ => serve_not_found(connection).await,
        }
    }
}

/// Build the Prometheus body and send it with an accurate `Content-Length`.
async fn serve_metrics<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut body_buffer = METRICS_BODY_BUFFER.lock().await;
    // Render under the map lock, then release it before the socket write
    // so a slow scrape blocks only other scrapes, never ingestion.
    let body_len = {
        let map = METRIC_MAP.lock().await;
        build_prometheus_body(&mut body_buffer, &map)
    };

    let content_length = format_content_length(body_len);

    connection
        .initiate_response(
            200,
            Some("OK"),
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
        404,
        "Not Found",
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
        405,
        "Method Not Allowed",
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
/// warning ever fires, raise `Metrics::RESPONSE_BUFFER_SIZE` or trim
/// the metric set.
fn build_prometheus_body(
    buffer: &mut [u8; Metrics::RESPONSE_BUFFER_SIZE],
    map: &heapless::index_map::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
) -> usize {
    let mut writer = BufferWriter::new(buffer);

    // HELP/TYPE lines are omitted throughout — they are optional in the
    // Prometheus text format and the buffer is tight.
    for (key, stored) in map {
        render_metric(&mut writer, key.0.as_str(), stored);
    }

    // Render hardcoded derived metrics.
    let uptime_secs = Instant::now().as_secs();
    let ntp_offset = ubos_net::net::handle().ntp_epoch_offset();
    let ts_ms = if ntp_offset == 0 {
        None
    } else {
        Some((uptime_secs + ntp_offset) * 1000)
    };

    render_scalar(&mut writer, "board_uptime_seconds", uptime_secs, ts_ms);

    let request_count = ubos_net::net::handle().request_count();
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
             (raise Metrics::RESPONSE_BUFFER_SIZE or reduce series count)"
        );
    }
    writer.len()
}

/// Render one stored metric line. The key string includes name and labels.
fn render_metric(writer: &mut BufferWriter<'_>, key: &str, stored: &StoredValue) {
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
    let ntp_offset = ubos_net::net::handle().ntp_epoch_offset();
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
fn render_float_value(writer: &mut BufferWriter<'_>, value: f32) {
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
