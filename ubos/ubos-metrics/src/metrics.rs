//! Prometheus metrics exposition server running on [`Metrics::PORT`].
//!
//! Endpoint model: `../../../docs/METRICS.md`.

use core::fmt::Write as _;

use edge_http::io::server::{handle_connection, Connection, Handler};
use edge_http::io::Error as HttpError;
use edge_http::Method;
use edge_nal::TcpSplit;
use edge_nal::{TcpAccept as _, TcpBind as _};
use edge_nal_embassy::{Tcp, TcpBuffers};

use embassy_futures::select::{select, Either};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_time::{with_timeout, Instant};
use embedded_io_async::{Read, Write};
use portable_atomic::Ordering;
use static_cell::ConstStaticCell;
use ubos_logging::{warn, Debug2Format};

use ubos_core::consts::{Metrics, Net};
use ubos_http::{format_content_length, BufferWriter, CONTENT_LENGTH_DECIMAL_DIGITS};
use ubos_net::net::{BTC_PRICE_CENTS, LAST_BTC_FETCH_UPTIME_SECS, NTP_EPOCH_OFFSET, REQUEST_COUNT};

/// Maximum number of HTTP headers parsed per metrics request.
///
/// Prometheus scrapers send roughly six headers (Host, User-Agent,
/// Accept, Accept-Encoding, X-Prometheus-Scrape-Timeout-Seconds, and
/// Connection); sixteen slots fit all observed headers with room for
/// future additions without inflating the per-connection stack frame.
const MAX_HEADERS: usize = 16;

/// Logging task identifier passed to `edge_http::io::server::handle_connection`.
///
/// The accept loop serves one connection at a time; edge-http only
/// uses this value to tag log lines.
const TASK_ID: usize = 0;

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
/// Budget: 49 bytes covers the longest emitted key,
/// `board_lm75a_temperature_millicelsius{addr="0x4c"}`, including the
/// full label-expanded form with braces and quoted value. The remaining
/// 15 bytes fit one additional short label before `upsert_metric` hits
/// the cap and emits a `warn!`. If a new long-name metric is added,
/// raise `KEY_CAPACITY` instead of relying on the runtime drop.
const KEY_CAPACITY: usize = 64;

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

/// Accepted-connection pool depth for the metrics server. Raising this
/// breaks the `METRICS_BODY_BUFFER` single-writer assumption below; the
/// `const _` assertion next to that buffer enforces the coupling at
/// compile time.
const METRICS_TCP_POOL_DEPTH: usize = 1;

static METRICS_TCP_BUFFERS: ConstStaticCell<
    TcpBuffers<METRICS_TCP_POOL_DEPTH, { Metrics::TX_BUFFER_SIZE }, { Metrics::RX_BUFFER_SIZE }>,
> = ConstStaticCell::new(TcpBuffers::new());

#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static METRICS_REQUEST: ConstStaticCell<[u8; Metrics::REQUEST_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Metrics::REQUEST_BUFFER_SIZE]);

/// Hoisted `FnvIndexMap` so it can be placed in CCM RAM on boards that
/// enable the `ccmram-bss` feature. Embedding the map inside the
/// async `run` future would force it into the standard `.bss` along
/// with the rest of the task state, which is exactly what blew the
/// stm32f4 main SRAM during the api refactor.
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static METRIC_MAP: ConstStaticCell<
    heapless::index_map::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
> = ConstStaticCell::new(heapless::index_map::FnvIndexMap::new());

/// Hoisted exposition body buffer. Lives next to `METRIC_MAP` so the
/// 2 KiB scratch does not ride on the metrics-task async frame.
///
/// Wrapped in `Mutex` rather than `ConstStaticCell`+`RefCell` so the
/// lock guard can be held across `await` points during the response
/// write. Sound only because the `TcpBuffers` pool depth is 1, so
/// `serve_metrics` cannot be re-entered concurrently; if pool depth is
/// ever raised, scrapes will serialize on this lock.
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static METRICS_BODY_BUFFER: Mutex<CriticalSectionRawMutex, [u8; Metrics::RESPONSE_BUFFER_SIZE]> =
    Mutex::new([0; Metrics::RESPONSE_BUFFER_SIZE]);

const _: () = assert!(
    METRICS_TCP_POOL_DEPTH == 1,
    "METRICS_BODY_BUFFER is a single Mutex shared across all accepted \
     connections; raising the pool depth requires either making the buffer \
     per-slot or accepting that concurrent scrapes will serialize on this \
     lock."
);

/// Push a metric into the exposition channel. Awaits if the channel is full.
pub async fn push(metric: Metric) {
    METRICS.send(metric).await;
}

/// Serve Prometheus exposition on [`Metrics::PORT`] and consume pushed metrics.
///
/// Call this from a firmware `#[embassy_executor::task]`.
///
/// # Panics
///
/// Panics if `TcpBind::bind` returns an error. The current
/// `edge-nal-embassy` implementation of `bind` is infallible and
/// only stores the address; a panic here would indicate a library
/// contract violation.
pub async fn run(stack: Stack<'static>) -> ! {
    let tcp_buffers = METRICS_TCP_BUFFERS.take();
    let work_buffer = METRICS_REQUEST.take();
    let map = METRIC_MAP.take();

    let tcp = Tcp::new(stack, tcp_buffers);

    let bind_addr = core::net::SocketAddr::new(
        core::net::IpAddr::V4(core::net::Ipv4Addr::UNSPECIFIED),
        Metrics::PORT,
    );

    #[expect(
        clippy::expect_used,
        reason = "TcpBind::bind on edge-nal-embassy is infallible"
    )]
    // edge-nal-embassy TcpBind::bind() is documented infallible;
    // a failure here is a library contract violation.
    let acceptor = tcp
        .bind(bind_addr)
        .await
        .expect("BUG: metrics TCP bind failed");

    loop {
        match select(acceptor.accept(), METRICS.receive()).await {
            Either::Second(metric) => {
                upsert_metric(map, &metric);
            }
            Either::First(Err(error)) => {
                warn!("metrics tcp accept error: {:?}", Debug2Format(&error));
            }
            Either::First(Ok((_peer_addr, socket))) => {
                // Drain pending metrics so the response reflects the latest values.
                while let Ok(metric) = METRICS.try_receive() {
                    upsert_metric(map, &metric);
                }
                let handler = MetricsHandler {
                    map,
                    body_buffer: &METRICS_BODY_BUFFER,
                };
                // Cap per-connection lifetime so a stalled scraper
                // cannot hold the single accept slot indefinitely,
                // which would starve subsequent scrapes and the
                // metric upserts driven from this select loop.
                if with_timeout(
                    Net::SOCKET_TIMEOUT,
                    handle_connection::<_, _, MAX_HEADERS>(
                        socket,
                        work_buffer,
                        None,
                        TASK_ID,
                        handler,
                    ),
                )
                .await
                .is_err()
                {
                    warn!("metrics connection timed out");
                }
            }
        }
    }
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

/// edge-http handler for the Prometheus `/metrics` endpoint.
///
/// Holds a mutable reference to the metric map and a reference to the
/// hoisted exposition body buffer. One `MetricsHandler` is constructed
/// per accepted connection and dropped when `handle_connection`
/// returns. The body buffer is reached through a `Mutex` so its guard
/// can be held across `await` points during the response write.
struct MetricsHandler<'a> {
    map: &'a mut heapless::index_map::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
    body_buffer: &'a Mutex<CriticalSectionRawMutex, [u8; Metrics::RESPONSE_BUFFER_SIZE]>,
}

impl Handler for MetricsHandler<'_> {
    type Error<E>
        = HttpError<E>
    where
        E: core::fmt::Debug;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl core::fmt::Display + Copy,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + TcpSplit,
    {
        let (is_get, path) = {
            let headers = connection.headers().map_err(|_| HttpError::InvalidState)?;

            // Compare method before the borrow on `headers` ends; the
            // result is a plain bool that does not hold any reference.
            let is_get = headers.method == Method::Get;

            let mut path_buf = heapless::String::<128>::new();
            let path_too_long = headers.path.len() > 128;
            if path_too_long {
                warn!(
                    "metrics: request path {} bytes exceeds 128 cap; replaced with `/`",
                    headers.path.len()
                );
            }
            let _ = path_buf.push_str(if path_too_long { "/" } else { headers.path });

            (is_get, path_buf)
        };

        match (is_get, path.as_str()) {
            (true, "/metrics") => self.serve_metrics(connection).await,
            (false, "/metrics") => serve_method_not_allowed(connection).await,
            _ => serve_not_found(connection).await,
        }
    }
}

impl MetricsHandler<'_> {
    /// Build the Prometheus body and send it with an accurate `Content-Length`.
    async fn serve_metrics<T, const N: usize>(
        &self,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), HttpError<T::Error>>
    where
        T: Read + Write + TcpSplit,
    {
        let mut body_buffer = self.body_buffer.lock().await;
        let body_len = build_prometheus_body(&mut body_buffer, self.map);

        let mut length_buffer = [0u8; CONTENT_LENGTH_DECIMAL_DIGITS];
        let content_length = format_content_length(&mut length_buffer, body_len);

        connection
            .initiate_response(
                200,
                Some("OK"),
                &[
                    ("Content-Type", "text/plain; version=0.0.4"),
                    ("Content-Length", content_length),
                    ("Connection", "close"),
                ],
            )
            .await?;

        embedded_io_async::Write::write_all(connection, &body_buffer[..body_len]).await?;

        Ok(())
    }
}

/// Write the 404 response.
async fn serve_not_found<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ubos_http::write_status_only(connection, 404, "Not Found", "text/plain", &[], b"").await
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
        &[("Allow", "GET")],
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

    // Render map-stored metrics. HELP/TYPE lines are omitted — they are
    // optional in the Prometheus text format.
    for (key, stored) in map {
        render_metric(&mut writer, key.0.as_str(), stored);
    }

    // Render hardcoded derived metrics.
    let uptime_secs = Instant::now().as_secs();
    let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    let ts_ms = if ntp_offset == 0 {
        None
    } else {
        Some((uptime_secs + ntp_offset) * 1000)
    };

    let _ = writeln!(writer, "# HELP board_uptime_seconds Seconds since boot.");
    let _ = writeln!(writer, "# TYPE board_uptime_seconds gauge");
    render_scalar(&mut writer, "board_uptime_seconds", uptime_secs, ts_ms);

    let request_count = REQUEST_COUNT.load(Ordering::Relaxed);
    let _ = writeln!(
        writer,
        "# HELP board_http_requests_total Accepted connections on the HTTP port, regardless of response status."
    );
    let _ = writeln!(writer, "# TYPE board_http_requests_total counter");
    render_scalar(
        &mut writer,
        "board_http_requests_total",
        request_count,
        ts_ms,
    );

    let btc_cents = BTC_PRICE_CENTS.load(Ordering::Relaxed);
    if btc_cents != 0 {
        let _ = writeln!(
            writer,
            "# HELP board_btc_price_usd Last successful BTC/USDT price from Binance in USD."
        );
        let _ = writeln!(writer, "# TYPE board_btc_price_usd gauge");
        let dollars = btc_cents / 100;
        let cents = btc_cents % 100;
        let _ = write!(
            writer,
            "board_btc_price_usd{{source=\"binance\"}} {dollars}.{cents:02}"
        );
        if let Some(ts_ms) = ts_ms {
            let _ = write!(writer, " {ts_ms}");
        }
        let _ = writeln!(writer);
    }

    let last_fetch = LAST_BTC_FETCH_UPTIME_SECS.load(Ordering::Relaxed);
    if last_fetch != 0 {
        let _ = writeln!(
            writer,
            "# HELP board_btc_price_age_seconds Seconds since the last successful BTC price fetch."
        );
        let _ = writeln!(writer, "# TYPE board_btc_price_age_seconds gauge");
        // Age is uptime-based so NTP sync shifts don't produce phantom gaps.
        let age = Instant::now().as_secs().saturating_sub(last_fetch);
        let _ = write!(
            writer,
            "board_btc_price_age_seconds{{source=\"binance\"}} {age}"
        );
        if let Some(ts_ms) = ts_ms {
            let _ = write!(writer, " {ts_ms}");
        }
        let _ = writeln!(writer);
    }

    let _ = writeln!(
        writer,
        "# HELP board_ntp_synced 1 if NTP has synced, 0 otherwise."
    );
    let _ = writeln!(writer, "# TYPE board_ntp_synced gauge");
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
    let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
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
