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
/// Fields are private; construct through [`Metric::with_no_labels`],
/// [`Metric::with_labels`] or [`Metric::indexed`], each of which stamps
/// the metric with the current monotonic time, and push it with
/// [`Metric::push`].
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
/// Each byte costs one byte per map slot, so a board pays it
/// `MAX_METRICS` times over and this is raised deliberately rather than
/// rounded up. A metric longer than the constant below must move both,
/// and the test beside it is what catches one that does not.
const KEY_CAPACITY: usize = 80;

/// The longest key the tree renders, in the label-expanded form
/// [`MetricKey::from_metric`] builds: `board_temperature_millicelsius`
/// with the `addr`, `chain` and `source` labels `ubos-thermal` gives a
/// hashboard reading. A chain index is one digit for every `MAX_CHAINS`
/// below ten, and a 7-bit address renders in four characters.
const LONGEST_RENDERED_KEY: &str =
    "board_temperature_millicelsius{addr=\"0x48\",chain=\"0\",source=\"hashboard\"}";

const _: () = assert!(
    LONGEST_RENDERED_KEY.len() <= KEY_CAPACITY,
    "KEY_CAPACITY is below the longest key a metric renders"
);

/// Characters of the widest integer a line carries, the digits of
/// `u64::MAX`. `i64::MIN` is one digit shorter and takes that place with
/// its sign.
const MAX_INTEGER_CHARS: usize = u64::MAX.ilog10() as usize + 1;

/// Bytes the longest series line takes in a scrape: the key, a space, an
/// integer value, a space, the millisecond timestamp, and the newline. A
/// board sizes its render buffer as this times its series cap, so the
/// buffer holds a full map.
pub const METRIC_LINE_BYTES: usize =
    KEY_CAPACITY + 1 + MAX_INTEGER_CHARS + 1 + MAX_INTEGER_CHARS + 1;

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

/// The exposition map and its render buffer, sized by the board.
///
/// `MAX` is the number of distinct metric series the map holds and must
/// be a power of two, the heapless `FnvIndexMap` invariant. The map is
/// keyed by the fully label-expanded series, so per-CPU and per-LM75A
/// label combinations each consume one slot; a board with three chains
/// and three pools pushes about 90. `BODY` is the exposition body
/// buffer, which has to hold every rendered series at once.
///
/// Both are a board's RAM budget rather than a property of this crate,
/// so a board states them in its own `config.rs` and
/// `ubos_firmware::serve_all!` declares the static from there. See the
/// Footprint section of `docs/STRUCTURE.md` for the per-board snapshot.
///
/// The two live in one static so a board places them together: they are
/// hoisted out of the task futures that would otherwise carry them,
/// which is what blew the stm32f4 main SRAM during the api refactor.
pub struct MetricsStatics<const MAX: usize, const BODY: usize> {
    /// Current metric values, shared between [`run_ingest`] (the sole
    /// writer) and the exposition workers (readers).
    map: Mutex<
        CriticalSectionRawMutex,
        heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    >,
    /// Exposition render buffer.
    ///
    /// A single `Mutex` shared across all exposition workers: with a
    /// pool depth above one, concurrent scrapes serialize on this lock
    /// while rendering. [`serve_metrics`] drops the map guard before the
    /// socket write, so a slow scrape blocks only other scrapes, never
    /// the ingestion task.
    body: Mutex<CriticalSectionRawMutex, [u8; BODY]>,
}

impl<const MAX: usize, const BODY: usize> MetricsStatics<MAX, BODY> {
    /// An empty map and a zeroed render buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            map: Mutex::new(heapless::index_map::FnvIndexMap::new()),
            body: Mutex::new([0; BODY]),
        }
    }
}

impl<const MAX: usize, const BODY: usize> Default for MetricsStatics<MAX, BODY> {
    fn default() -> Self {
        Self::new()
    }
}

/// Declare one [`MetricsStatics`] in CCM RAM.
///
/// Mirrors `ubos_http::http_pool_static!`, and for the same reason: the
/// `ccmram-bss` feature has to be read where it is declared. A
/// `cfg_attr` written inside a macro body resolves against the crate the
/// macro expands into, which is the board, so the test is made here at
/// definition and the expansion carries no `cfg` of its own.
#[cfg(feature = "ccmram-bss")]
#[macro_export]
macro_rules! metrics_statics {
    ($name:ident, $max:expr, $body:expr) => {
        #[unsafe(link_section = ".ccmram.bss")]
        static $name: $crate::metrics::MetricsStatics<{ $max }, { $body }> =
            $crate::metrics::MetricsStatics::new();
    };
}

/// Declare one [`MetricsStatics`] in main SRAM. See the `ccmram-bss`
/// variant.
#[cfg(not(feature = "ccmram-bss"))]
#[macro_export]
macro_rules! metrics_statics {
    ($name:ident, $max:expr, $body:expr) => {
        static $name: $crate::metrics::MetricsStatics<{ $max }, { $body }> =
            $crate::metrics::MetricsStatics::new();
    };
}

/// Push a metric into the exposition channel. Awaits if the channel is full.
pub async fn push(metric: Metric) {
    METRICS.send(metric).await;
}

/// Push one metric carrying one `<label>="<index>"` tag per entry of
/// `indices`, in the order given.
///
/// Delegates to [`Metric::indexed`] so the callers still on this function
/// keep one label shape until they move to the method.
pub async fn push_indexed_metrics(
    name: &'static str,
    indices: &[(&'static str, usize)],
    value: Value,
) {
    Metric::indexed(name, indices, value).push().await;
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

/// Consume pushed metrics into the shared map.
///
/// The sole writer of `statics`' map. `ubos_firmware::serve_all!` wraps
/// this in the board's `#[embassy_executor::task]`; the exposition
/// workers it spawns beside that task read the map through
/// [`PrometheusMetricsService`], so the exposition always reflects the
/// latest pushed values without a drain step on the serve path.
pub async fn run_ingest<const MAX: usize, const BODY: usize>(
    statics: &'static MetricsStatics<MAX, BODY>,
) -> ! {
    loop {
        let metric = METRICS.receive().await;
        let mut map = statics.map.lock().await;
        upsert_metric(&mut map, &metric);
    }
}

/// Upsert a metric into the map, sorting its labels into a canonical key.
fn upsert_metric<const MAX: usize>(
    map: &mut heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
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
/// One [`Service`] shared across all exposition workers. It reads the
/// map [`run_ingest`] keeps current and renders into the render buffer
/// beside it, both owned by the board's [`MetricsStatics`].
pub struct PrometheusMetricsService<const MAX: usize, const BODY: usize> {
    statics: &'static MetricsStatics<MAX, BODY>,
}

impl<const MAX: usize, const BODY: usize> PrometheusMetricsService<MAX, BODY> {
    /// Serve the exposition out of `statics`.
    #[must_use]
    pub const fn new(statics: &'static MetricsStatics<MAX, BODY>) -> Self {
        Self { statics }
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
            (true, "/metrics") => serve_metrics(self.statics, connection).await,
            (false, "/metrics") => serve_method_not_allowed(connection).await,
            _ => serve_not_found(connection).await,
        }
    }
}

/// Build the Prometheus body and send it with an accurate `Content-Length`.
async fn serve_metrics<T, const N: usize, const MAX: usize, const BODY: usize>(
    statics: &'static MetricsStatics<MAX, BODY>,
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
/// warning ever fires, raise the board's `METRICS_RESPONSE_BYTES` or
/// trim the metric set.
fn build_prometheus_body<const MAX: usize, const BODY: usize>(
    buffer: &mut [u8; BODY],
    map: &heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
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
             (raise the board's METRICS_RESPONSE_BYTES or reduce series count)"
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
