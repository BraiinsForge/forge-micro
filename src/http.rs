//! HTTP/1.0 response builder for the single metrics endpoint.  Reads
//! every time-varying value from the shared atomics so the HTTP page
//! stays in sync with the display and the caller does not have to load
//! the same statics twice per request.  Called by
//! [`crate::net::run_http_server`] for every request.

use core::fmt::{self, Write};

use embassy_net::Ipv4Address;
use portable_atomic::Ordering;

// `hashrate::hashrate` needs the difficulty target to convert the share
// delta into Hz; the dummy_asic test is the only share-producing path
// here, so its target constant is the correct argument. The cpu-mining
// path bypasses this conversion via `record_external_rate`, which is
// why the import is unconditional rather than feature-gated.
use crate::dummy_asic::TEST_TARGET_LEADING_ZERO_BYTES;
use crate::hashrate::{self, SHARE_COUNT};
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
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buffer.len() - self.position;
        if bytes.len() > remaining {
            return Err(fmt::Error);
        }
        self.buffer[self.position..self.position + bytes.len()].copy_from_slice(bytes);
        self.position += bytes.len();
        Ok(())
    }
}

/// Build an HTTP/1.0 200 response with an HTML metrics page.
///
/// Returns the number of bytes written into `buffer`.  Per-request
/// inputs (`uptime_secs`, `request_count`) come from the caller; every
/// time-varying shared value (IP, BTC price, NTP offset, hashrate) is
/// loaded from its respective atomic so the page matches what the
/// display is showing at the same instant.
pub fn build_metrics_response(
    buffer: &mut [u8],
    board_name: &str,
    uptime_secs: u64,
    request_count: u32,
) -> usize {
    let mut writer = BufferWriter::new(buffer);
    // Silently truncates if the response exceeds the buffer capacity.
    let _ = write!(
        writer,
        "HTTP/1.0 200 OK\r\n\
         Content-Type: text/html\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let _ = write!(
        writer,
        "<!DOCTYPE html><html><head><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><style>body{{background:#1a1b26;color:#a9b1d6;font-family:monospace;margin:2em}}h1{{color:#7aa2f7}}.v{{color:#9ece6a}}.btc{{color:#e0af68}}</style></head>"
    );
    let _ = write!(writer, "<body><h1>{board_name}</h1>");

    let ip_bits = LOCAL_IPV4.load(Ordering::Relaxed);
    if ip_bits != 0 {
        let octets = Ipv4Address::from_bits(ip_bits).octets();
        let _ = write!(
            writer,
            "<p>IP: <span class=\"v\">{}.{}.{}.{}</span></p>",
            octets[0], octets[1], octets[2], octets[3]
        );
    }

    let _ = write!(
        writer,
        "<p>Uptime: <span class=\"v\">{uptime_secs}</span>s</p>"
    );
    let _ = write!(
        writer,
        "<p>Requests: <span class=\"v\">{request_count}</span></p>"
    );

    // Difficulty target is hard-coded to TEST_TARGET_LEADING_ZERO_BYTES
    // because dummy_asic::build_work_frame always issues Work at that
    // target; if a future caller pushes a different target the exposed
    // rate will silently misreport.
    let hashrate = hashrate::hashrate(TEST_TARGET_LEADING_ZERO_BYTES);
    let shares = SHARE_COUNT.load(Ordering::Relaxed);
    let _ = write!(
        writer,
        "<p>Hashrate: <span class=\"v\">{hashrate}</span> H/s ({shares} shares)</p>"
    );

    let cents = BTC_PRICE_CENTS.load(Ordering::Relaxed);
    if cents != 0 {
        let dollars = cents / 100;
        let remainder = cents % 100;
        let _ = write!(
            writer,
            "<p>BTC: <span class=\"btc\">${dollars}.{remainder:02}</span></p>"
        );
    }

    let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    if ntp_offset != 0 {
        let unix_secs = crate::wall_clock_secs();
        let mut time_buffer = [0u8; 16];
        let length = crate::ntp::format_utc_time(&mut time_buffer, unix_secs);
        if length > 0 {
            if let Ok(time_str) = core::str::from_utf8(&time_buffer[..length]) {
                let _ = write!(writer, "<p>Time: <span class=\"v\">{time_str}</span></p>");
            }
        }
    }

    let _ = write!(writer, "</body></html>");
    writer.len()
}
