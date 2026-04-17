//! HTTP/1.0 response builder for the single metrics endpoint. Formats
//! board name, uptime, request count, BTC price, and UTC time into a
//! caller-owned byte buffer with `core::fmt`, returning the number of
//! bytes written. Called by [`crate::net::run_http_server`] for every request.

use core::fmt::{self, Write};

/// Wrapper around a byte slice that tracks the write position.
struct BufferWriter<'a> {
    buffer: &'a mut [u8],
    position: usize,
}

impl<'a> BufferWriter<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            position: 0,
        }
    }

    #[must_use]
    fn len(&self) -> usize {
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
/// Returns the number of bytes written into `buffer`.
pub fn build_metrics_response(
    buffer: &mut [u8],
    board_name: &str,
    uptime_secs: u64,
    request_count: u32,
    btc_price_cents: Option<u32>,
    utc_time_secs: Option<u64>,
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
    let _ = write!(
        writer,
        "<p>Uptime: <span class=\"v\">{uptime_secs}</span>s</p>"
    );
    let _ = write!(
        writer,
        "<p>Requests: <span class=\"v\">{request_count}</span></p>"
    );
    if let Some(cents) = btc_price_cents {
        let dollars = cents / 100;
        let remainder = cents % 100;
        let _ = write!(
            writer,
            "<p>BTC: <span class=\"btc\">${dollars}.{remainder:02}</span></p>"
        );
    }
    if let Some(secs) = utc_time_secs {
        let mut time_buffer = [0u8; 16];
        let length = crate::ntp::format_utc_time(&mut time_buffer, secs);
        if let Ok(time_str) = core::str::from_utf8(&time_buffer[..length]) {
            let _ = write!(writer, "<p>Time: <span class=\"v\">{time_str}</span></p>");
        }
    }
    let _ = write!(writer, "</body></html>");
    writer.len()
}
