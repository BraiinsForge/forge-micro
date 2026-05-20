//! HTTP/1.0 response builders for the firmware web UI.
//!
//! Four endpoints share the socket loop in [`run_http_server`]:
//!
//! - `GET /` returns the inlined HTML dashboard. The page is fully
//!   static — every time-varying field is filled in client-side from
//!   `/api/info`. Hash-routed single-page app with a top nav bar, a
//!   dual-axis hashrate/temperature chart, a right-side telemetry
//!   panel, and a configuration form view. Inlined CSS and JS keep
//!   the first paint to a single response, well under the TCP initial
//!   congestion window for one-RTT delivery on LAN.
//! - `GET /logo.svg` serves the Braiins wordmark referenced by the
//!   dashboard's nav bar. Kept as a separate cacheable asset rather
//!   than inlined so the index response stays inside its buffer.
//! - `GET /api/info` returns a compact JSON telemetry blob. The
//!   dashboard polls it every 2 s and pushes hashrate and temperature
//!   samples into ring buffers rendered as SVG polylines.
//! - `POST /api/config` accepts the dashboard's pool-config form. The
//!   router logs the submitted body and returns a stub 200 OK; no
//!   firmware state changes yet.
//!
//! Any other path gets a small 404. Responses are written into a
//! caller-owned buffer sized in [`ubos_core::consts::Http::RESPONSE_BUFFER_SIZE`].

use core::fmt::{self, Write};

use embassy_net::tcp::TcpSocket;
use embassy_net::{Ipv4Address, Stack};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write as _;
use portable_atomic::Ordering;
use static_cell::ConstStaticCell;
use ubos_core::consts::{Http, Net};
use ubos_core::fan_state::{FAN_PWM_PERCENT, FAN_RPM};
use ubos_logging::{info, warn};
use ubos_net::net::{
    LOCAL_IPV4, NTP_EPOCH_OFFSET, POOL_ALIVE_BITMAP, REQUEST_COUNT, TEMPERATURE_C,
};

/// Wrapper around a byte slice that tracks the write position.
pub(crate) struct BufferWriter<'a> {
    buffer: &'a mut [u8],
    position: usize,
}

impl<'a> BufferWriter<'a> {
    pub(crate) fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            position: 0,
        }
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.position
    }

    /// Append raw bytes to the buffer. Used by static-asset routes
    /// such as the logo where the payload is not valid UTF-8 from
    /// the writer's point of view but still needs the same overflow
    /// handling as text writes.
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> fmt::Result {
        let remaining = self.buffer.len() - self.position;
        if bytes.len() > remaining {
            return Err(fmt::Error);
        }
        self.buffer[self.position..self.position + bytes.len()].copy_from_slice(bytes);
        self.position += bytes.len();
        Ok(())
    }
}

impl Write for BufferWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_bytes(text.as_bytes())
    }
}

/// Gzipped dashboard payload, produced from `src/index.html` by the
/// crate's `build.rs`. Served with `Content-Encoding: gzip` so a fresh
/// connection paints in one RTT well inside the TCP initial congestion
/// window even as the SPA grows. Every browser in the field already
/// supports the encoding.
const INDEX_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

#[cfg(test)]
#[path = "tests/http.rs"]
mod tests;

/// Braiins wordmark served at `/logo.svg`. Kept as a separate asset
/// rather than inlined into `index.html` so the dashboard page stays
/// inside the response buffer budget and the browser can cache the
/// logo across reloads.
const LOGO_SVG: &[u8] = include_bytes!("logo.svg");

/// Build an HTTP/1.0 response delivering the inlined dashboard.
///
/// Returns the number of bytes written. The page is fully static and
/// served with `Cache-Control: no-store` so a firmware update reflects
/// immediately on next reload.
pub fn build_index_response(buffer: &mut [u8]) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Encoding: gzip\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let _ = writer.write_bytes(INDEX_HTML);
    writer.len()
}

/// Build an HTTP/1.0 JSON response for the `/api/info` endpoint.
///
/// The dashboard polls this every two seconds. Optional fields (`ip`,
/// `time`) appear as empty strings when the underlying state has not
/// been initialized; the client treats those as "not yet available".
///
/// `board_name` is interpolated unescaped into the JSON `"name"` field;
/// callers must keep it free of `"` and `\`.
pub fn build_api_info_response(buffer: &mut [u8], board_name: &str, uptime_secs: u64) -> usize {
    debug_assert!(
        !board_name.contains(['"', '\\']),
        "board_name must be JSON-safe; contains `\"` or `\\`",
    );

    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 200 OK\r\n\
         Content-Type: application/json\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n"
    );

    let hashrate = crate::hashrate::hashrate();
    let temperature = TEMPERATURE_C.load(Ordering::Relaxed);
    let fan_rpm = FAN_RPM.load(Ordering::Relaxed);
    // `ubos_core::fan_state::FAN_PWM_PERCENT` uses `u8::MAX` to mean
    // "no fan on this board"; the dashboard treats zero as "not
    // available", so map the sentinel to zero before serializing.
    let fan_pwm = match FAN_PWM_PERCENT.load(Ordering::Relaxed) {
        u8::MAX => 0,
        percent => percent,
    };
    let pools = POOL_ALIVE_BITMAP.load(Ordering::Relaxed);

    let _ = write!(
        writer,
        "{{\"name\":\"{board_name}\",\
         \"hashrate\":{hashrate},\
         \"uptime\":{uptime_secs},\
         \"temperature\":{temperature},\
         \"fan_rpm\":{fan_rpm},\
         \"fan_pwm\":{fan_pwm},\
         \"pools\":{pools},\
         \"ip\":\""
    );

    let ip_bits = LOCAL_IPV4.load(Ordering::Relaxed);
    if ip_bits != 0 {
        let octets = Ipv4Address::from_bits(ip_bits).octets();
        let _ = write!(
            writer,
            "{}.{}.{}.{}",
            octets[0], octets[1], octets[2], octets[3]
        );
    }

    let _ = writer.write_str("\",\"time\":\"");
    let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    if ntp_offset != 0 {
        let unix_secs = ubos_net::wall_clock_secs();
        let mut time_buffer = [0u8; 16];
        let length = ubos_net::ntp::format_utc_time(&mut time_buffer, unix_secs);
        if let Ok(time_str) = core::str::from_utf8(&time_buffer[..length]) {
            let _ = writer.write_str(time_str);
        }
    }
    let _ = writer.write_str("\"}");

    writer.len()
}

/// Build an HTTP/1.0 response delivering the Braiins wordmark SVG.
///
/// Sets a long `Cache-Control` so the logo survives reloads; the
/// asset never changes between firmware versions in practice, and
/// keeping it out of the dashboard response makes room for the SPA
/// to grow without bumping the response buffer again.
pub fn build_logo_response(buffer: &mut [u8]) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 200 OK\r\n\
         Content-Type: image/svg+xml\r\n\
         Cache-Control: public, max-age=86400\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let _ = writer.write_bytes(LOGO_SVG);
    writer.len()
}

/// Build a small HTTP/1.0 200 OK acknowledgement for the config POST.
///
/// The route handler logs the submitted body and then writes this
/// response. No state changes on the firmware side yet; this is the
/// scaffolding for the eventual pool-config commit handler.
pub fn build_config_ack_response(buffer: &mut [u8]) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 200 OK\r\n\
         Content-Type: application/json\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {{\"ok\":true}}"
    );
    writer.len()
}

/// Build a small HTTP/1.0 404 response for unknown paths.
pub fn build_not_found_response(buffer: &mut [u8]) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 404 Not Found\r\n\
         Content-Type: text/plain\r\n\
         Connection: close\r\n\
         \r\n\
         not found"
    );
    writer.len()
}

static HTTP_RX: ConstStaticCell<[u8; Http::RX_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Http::RX_BUFFER_SIZE]);
static HTTP_TX: ConstStaticCell<[u8; Http::TX_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Http::TX_BUFFER_SIZE]);
static HTTP_RESPONSE: ConstStaticCell<[u8; Http::RESPONSE_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Http::RESPONSE_BUFFER_SIZE]);

/// Accept HTTP connections and serve the dashboard. Runs forever.
///
/// Routes `GET /` to the inlined HTML dashboard, `GET /logo.svg` to
/// the cacheable Braiins wordmark, `GET /api/info` to a small JSON
/// telemetry blob the dashboard polls, and `POST /api/config` to a
/// stub handler that logs the submitted body before returning a 200
/// OK. Anything else gets a 404. A single TCP read is expected to
/// cover the request line and any small body; a well-behaved client
/// sends the whole request in one segment, and the config form fits
/// comfortably in [`Http::REQUEST_BUFFER_SIZE`].
pub async fn run_http_server(stack: Stack<'static>, board_name: &'static str) -> ! {
    let rx = HTTP_RX.take();
    let tx = HTTP_TX.take();
    let response = HTTP_RESPONSE.take();

    loop {
        let mut socket = TcpSocket::new(stack, rx, tx);
        socket.set_timeout(Some(Net::SOCKET_TIMEOUT));

        if let Err(error) = socket.accept(Http::PORT).await {
            warn!("tcp accept error: {:?}", error);
            continue;
        }

        let mut request_buffer = [0; Http::REQUEST_BUFFER_SIZE];
        let request_length = match socket.read(&mut request_buffer).await {
            Ok(0) | Err(_) => continue,
            Ok(length) => length,
        };

        REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
        let uptime_secs = Instant::now().as_secs();
        let path = parse_request_path(&request_buffer[..request_length]);
        let length = match path {
            Some("/") | None => self::build_index_response(response),
            Some("/api/info") => self::build_api_info_response(response, board_name, uptime_secs),
            Some("/logo.svg") => self::build_logo_response(response),
            Some("/api/config") => {
                let body = extract_request_body(&request_buffer[..request_length]);
                // `/api/config` is unauthenticated; truncate to keep a LAN peer
                // from spamming the defmt link with megabytes of attacker text.
                // Truncate the bytes before the UTF-8 check: slicing a `&str`
                // at the cap would panic on a multi-byte codepoint boundary,
                // while a chopped codepoint here just turns into Err and
                // falls back to `<non-utf8>`.
                let truncated = &body[..body.len().min(256)];
                let body_str = core::str::from_utf8(truncated).unwrap_or("<non-utf8>");
                info!("pool config update ({} bytes): {}", body.len(), body_str);
                self::build_config_ack_response(response)
            }
            Some(_) => self::build_not_found_response(response),
        };

        if let Err(error) = socket.write_all(&response[..length]).await {
            warn!("tcp write error: {:?}", error);
        }

        socket.flush().await.ok();
        socket.close();
        // Wait for the FIN handshake so the peer sees a clean close
        // instead of RST when the socket is dropped.
        Timer::after(Duration::from_millis(50)).await;
    }
}

/// Locate the body of an HTTP request inside the read buffer.
///
/// Returns the slice after the first `\r\n\r\n` header terminator, or an
/// empty slice if no terminator is present. Used by the config POST
/// route to surface the submitted JSON for logging; the caller is
/// responsible for trusting nothing about the body's validity.
fn extract_request_body(request: &[u8]) -> &[u8] {
    let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return &[];
    };
    &request[position + 4..]
}

/// Extract the request path from the first line of an HTTP request.
///
/// Expects `METHOD SP PATH SP HTTP/VERSION CRLF`. Returns `None` if the
/// line is malformed or not valid UTF-8. The method is not inspected,
/// so `POST /` or `PUT /api/info` are routed the same as the matching
/// `GET`. Acceptable here because all handlers are idempotent and
/// produce the same response body regardless of verb.
fn parse_request_path(request: &[u8]) -> Option<&str> {
    let line_end = request
        .iter()
        .position(|&byte| byte == b'\r' || byte == b'\n')?;
    let line = &request[..line_end];
    let first_space = line.iter().position(|&byte| byte == b' ')?;
    let after_method = &line[first_space + 1..];
    let second_space = after_method.iter().position(|&byte| byte == b' ')?;
    let path_bytes = &after_method[..second_space];
    core::str::from_utf8(path_bytes).ok()
}
