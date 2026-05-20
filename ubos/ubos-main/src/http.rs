//! HTTP/1.0 response builders for the firmware web UI.
//!
//! Four endpoints share the socket loop in [`crate::net::run_http_server`]:
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

use embassy_net::Ipv4Address;
use portable_atomic::Ordering;

use ubos_core::fan_state::{FAN_PWM_PERCENT, FAN_RPM};
use crate::net::{LOCAL_IPV4, NTP_EPOCH_OFFSET, POOL_ALIVE_BITMAP, TEMPERATURE_C};

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
    // `display::FAN_PWM_PERCENT` uses `u8::MAX` to mean "no fan on this
    // board"; the dashboard treats zero as "not available", so map the
    // sentinel to zero before serializing.
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
        let unix_secs = crate::wall_clock_secs();
        let mut time_buffer = [0u8; 16];
        let length = crate::ntp::format_utc_time(&mut time_buffer, unix_secs);
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
