//! HTTP/1.0 response builders for the firmware web UI.
//!
//! Two endpoints share the socket loop in [`crate::net::run_http_server`]:
//!
//! - `GET /` returns the inlined HTML dashboard. The page is fully
//!   static — every time-varying field is filled in client-side from
//!   `/api/info`. Card layout with a hashrate hero, a hand-rolled SVG
//!   polyline chart, and per-stat cards. Inlined CSS and JS keep the
//!   first paint to a single response, well under the TCP initial
//!   congestion window for one-RTT delivery on LAN.
//! - `GET /api/info` returns a compact JSON telemetry blob. The
//!   dashboard polls it every 2 s and pushes the hashrate sample into
//!   a 120-slot ring buffer rendered as the SVG polyline.
//!
//! Any other path gets a small 404. Responses are written into a
//! caller-owned buffer sized in [`crate::consts::Http::RESPONSE_BUFFER_SIZE`].

use core::fmt::{self, Write};

use embassy_net::Ipv4Address;
use portable_atomic::Ordering;

use crate::hashrate::SHARE_COUNT;
use crate::net::{BTC_PRICE_CENTS, LOCAL_IPV4, NTP_EPOCH_OFFSET};

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
}

impl Write for BufferWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let bytes = text.as_bytes();
        let remaining = self.buffer.len() - self.position;
        if bytes.len() > remaining {
            return Err(fmt::Error);
        }
        self.buffer[self.position..self.position + bytes.len()].copy_from_slice(bytes);
        self.position += bytes.len();
        Ok(())
    }
}

/// Inlined dashboard. Static HTML; all telemetry is fetched from
/// `/api/info` by the embedded JS and rendered into the card slots and
/// the SVG chart. Sized so the whole page fits in one response buffer
/// and arrives in a single RTT on LAN.
const INDEX_HTML: &str = include_str!("index.html");

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
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let _ = writer.write_str(INDEX_HTML);
    writer.len()
}

/// Build an HTTP/1.0 JSON response for the `/api/info` endpoint.
///
/// The dashboard polls this every two seconds. Optional fields (`ip`,
/// `time`) appear as empty strings and `btc_cents` is zero when the
/// underlying state has not been initialized; the client treats those
/// as "not yet available".
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
    let shares = SHARE_COUNT.load(Ordering::Relaxed);
    let cents = BTC_PRICE_CENTS.load(Ordering::Relaxed);

    let _ = write!(
        writer,
        "{{\"name\":\"{board_name}\",\
         \"hashrate\":{hashrate},\
         \"shares\":{shares},\
         \"uptime\":{uptime_secs},\
         \"btc_cents\":{cents},\
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

/// Build a small HTTP/1.0 404 response for unknown paths.
pub fn build_not_found_response(buffer: &mut [u8]) -> usize {
    let mut writer = BufferWriter::new(buffer);
    let _ = write!(
        writer,
        "HTTP/1.0 404 Not Found\r\n\
         Content-Type: text/plain\r\n\
         Connection: close\r\n\
         Content-Length: 9\r\n\
         \r\n\
         not found"
    );
    writer.len()
}
