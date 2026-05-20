//! Prometheus metrics exposition server running on [`Metrics::PORT`].
//!
//! Endpoint model: `../docs/METRICS.md`.

use core::fmt::Write as _;

use embassy_futures::select::{select, Either};
use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write;
use portable_atomic::Ordering;
use static_cell::ConstStaticCell;
use ubos_logging::warn;

use ubos_core::consts::Metrics;
use crate::http::BufferWriter;
use crate::net::{BTC_PRICE_CENTS, LAST_BTC_FETCH_UPTIME_SECS, NTP_EPOCH_OFFSET, REQUEST_COUNT};

/// A metric value.
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

/// A single metric observation to push into the exposition channel.
pub struct Metric {
    pub name: &'static str,
    pub labels: heapless::Vec<Label, { Metrics::MAX_LABELS }>,
    pub value: Value,
    /// Milliseconds since Unix epoch, or zero to omit the timestamp field.
    pub timestamp_ms: u64,
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
#[derive(PartialEq, Eq, Hash)]
struct MetricKey(heapless::String<128>);

impl MetricKey {
    /// Build a key from a metric, sorting its labels by name.
    fn from_metric(metric: &Metric) -> Option<Self> {
        let mut sorted_indices: [usize; Metrics::MAX_LABELS] = [0; Metrics::MAX_LABELS];
        let count = metric.labels.len();
        for (index, slot) in sorted_indices.iter_mut().enumerate().take(count) {
            *slot = index;
        }
        // Insertion sort is fine for the bounded label count.
        for outer in 1..count {
            let pivot = sorted_indices[outer];
            let mut inner = outer;
            while inner > 0
                && metric.labels[sorted_indices[inner - 1]].name > metric.labels[pivot].name
            {
                sorted_indices[inner] = sorted_indices[inner - 1];
                inner -= 1;
            }
            sorted_indices[inner] = pivot;
        }

        let mut key = heapless::String::<128>::new();
        key.push_str(metric.name).ok()?;
        if count > 0 {
            key.push('{').ok()?;
            for (position, &slot) in sorted_indices[..count].iter().enumerate() {
                if position > 0 {
                    key.push(',').ok()?;
                }
                key.push_str(metric.labels[slot].name).ok()?;
                key.push_str("=\"").ok()?;
                push_escaped_label_value(&mut key, metric.labels[slot].value.as_str())?;
                key.push('"').ok()?;
            }
            key.push('}').ok()?;
        }
        Some(MetricKey(key))
    }
}

/// Append a Prometheus-escaped label value to `key`. Returns `None` if the
/// buffer fills.
fn push_escaped_label_value(key: &mut heapless::String<128>, value: &str) -> Option<()> {
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

static METRICS_RX: ConstStaticCell<[u8; Metrics::RX_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Metrics::RX_BUFFER_SIZE]);
static METRICS_TX: ConstStaticCell<[u8; Metrics::TX_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Metrics::TX_BUFFER_SIZE]);
static METRICS_RESPONSE: ConstStaticCell<[u8; Metrics::RESPONSE_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Metrics::RESPONSE_BUFFER_SIZE]);
static METRICS_REQUEST: ConstStaticCell<[u8; Metrics::REQUEST_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Metrics::REQUEST_BUFFER_SIZE]);

/// Push a metric into the exposition channel. Awaits if the channel is full.
pub async fn push(metric: Metric) {
    METRICS.send(metric).await;
}

/// Serve Prometheus exposition on [`Metrics::PORT`] and consume pushed metrics.
///
/// Call this from a firmware `#[embassy_executor::task]`.
pub async fn run(stack: Stack<'static>) -> ! {
    let rx = METRICS_RX.take();
    let tx = METRICS_TX.take();
    let response = METRICS_RESPONSE.take();
    let request_buffer = METRICS_REQUEST.take();

    let mut map: heapless::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }> =
        heapless::FnvIndexMap::new();

    loop {
        let mut socket = TcpSocket::new(stack, rx, tx);
        socket.set_timeout(Some(ubos_core::consts::Net::SOCKET_TIMEOUT));

        let accept_future = socket.accept(Metrics::PORT);
        let receive_future = METRICS.receive();

        match select(accept_future, receive_future).await {
            Either::Second(metric) => {
                upsert_metric(&mut map, metric);
                continue;
            }
            Either::First(Err(error)) => {
                warn!("metrics tcp accept error: {:?}", error);
                continue;
            }
            Either::First(Ok(())) => {}
        }

        // Drain any metrics that arrived while the accept was pending.
        while let Ok(metric) = METRICS.try_receive() {
            upsert_metric(&mut map, metric);
        }

        let length = read_request(&mut socket, request_buffer).await;
        let path_ok = parse_metrics_request(&request_buffer[..length]);

        let response_len = if path_ok {
            build_prometheus_response(response, &map)
        } else {
            build_not_found_response(response)
        };

        if let Err(error) = socket.write_all(&response[..response_len]).await {
            warn!("metrics tcp write error: {:?}", error);
        }

        socket.flush().await.ok();
        socket.close();
        // Wait for the FIN handshake so the peer sees a clean close
        // instead of RST when the socket is dropped.
        Timer::after(Duration::from_millis(50)).await;
    }
}

/// Upsert a metric into the map, sorting its labels into a canonical key.
fn upsert_metric(
    map: &mut heapless::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
    metric: Metric,
) {
    let Some(key) = MetricKey::from_metric(&metric) else {
        warn!("metrics key encoding overflowed, dropping metric");
        return;
    };
    let stored = StoredValue {
        value: metric.value,
        timestamp_ms: metric.timestamp_ms,
    };
    if map.insert(key, stored).is_err() {
        warn!("metrics map full, dropping metric");
    }
}

/// Read up to [`Metrics::REQUEST_BUFFER_SIZE`] bytes or until `\r\n` appears.
///
/// Returns the number of bytes read.
async fn read_request(socket: &mut TcpSocket<'_>, buffer: &mut [u8]) -> usize {
    let mut total = 0;
    loop {
        match socket.read(&mut buffer[total..]).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                total += count;
                // Stop once we have a complete first line.
                if buffer[..total].windows(2).any(|window| window == b"\r\n") {
                    break;
                }
                if total >= buffer.len() {
                    break;
                }
            }
        }
    }
    total
}

/// Return true if the request is `GET /metrics HTTP/...`.
fn parse_metrics_request(buffer: &[u8]) -> bool {
    let Some(line_end) = buffer.windows(2).position(|window| window == b"\r\n") else {
        return false;
    };
    let Ok(line) = core::str::from_utf8(&buffer[..line_end]) else {
        return false;
    };
    let mut parts = line.splitn(3, ' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    // Prometheus scrapers do not attach query strings; exact match is enough.
    method == "GET" && path == "/metrics"
}

/// Write the Prometheus 404 response into `buffer`. Returns byte count.
fn build_not_found_response(buffer: &mut [u8]) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 404 Not Found\r\nConnection: close\r\n\r\n"
    );
    writer.len()
}

/// Write the Prometheus text exposition response into `buffer`. Returns byte count.
fn build_prometheus_response(
    buffer: &mut [u8],
    map: &heapless::FnvIndexMap<MetricKey, StoredValue, { Metrics::MAX_METRICS }>,
) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 200 OK\r\n\
         Content-Type: text/plain; version=0.0.4\r\n\
         Connection: close\r\n\
         \r\n"
    );

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
        "# HELP board_http_requests_total HTTP requests served on the HTML page."
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
        let price_usd = btc_cents / 100;
        let _ = write!(
            writer,
            "board_btc_price_usd{{source=\"binance\"}} {price_usd}"
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
    // Pushers that set ts_ms = 0 want the current wall-clock time at
    // scrape time so that prometheus doesn't drop the sample as
    // timestamp-less and still honours NTP sync for absolute time.
    let ts_ms = if stored.timestamp_ms == 0 {
        let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
        if ntp_offset == 0 {
            0
        } else {
            (Instant::now().as_secs() + ntp_offset) * 1000
        }
    } else {
        stored.timestamp_ms
    };
    if ts_ms != 0 {
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
