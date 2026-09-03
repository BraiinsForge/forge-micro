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

//! Shared HTTP response helpers for edge-http connections.
//!
//! Both helpers write directly to an edge-http `Connection`, filling
//! the gap in the edge-http API: the library ships no numeric
//! `Content-Length` header helper and no shared error-response writer.
//! Handler crates import these to avoid duplicating the same header
//! assembly logic.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use embedded_io_async::{Read, Write};
use heapless::String;

use edge_nal::TcpSplit;

#[cfg(test)]
#[path = "tests/response.rs"]
mod tests;

/// Maximum number of ASCII decimal digits needed for any `usize` value.
///
/// A 64-bit `usize` has at most 20 decimal digits. All targets in this
/// workspace are 32-bit, but the constant is sized for the widest
/// possible `usize` so the function is safe on any target without
/// revision.
pub const CONTENT_LENGTH_DECIMAL_DIGITS: usize = 20;

/// Maximum number of extra headers a caller may attach to a status
/// response on top of the two the helper writes itself
/// (`Content-Type`, `Content-Length`).
pub const MAX_EXTRA_STATUS_HEADERS: usize = 4;

/// Maximum number of response headers built per request.
///
/// Most responses use 3 headers (Content-Type, Cache-Control,
/// Content-Length). Session responses need an additional Set-Cookie or
/// X-CSRF-Token header. Eight slots cover the worst-case header count
/// (3 fixed + 1 encoding + 1 extra = 5) with three slots to spare.
pub const MAX_RESPONSE_HEADERS: usize = 8;

/// Write a complete HTTP status response to `connection`.
///
/// Sends `status` and `reason` as the status line, and adds the
/// caller-provided `content_type` and a `Content-Length` derived from
/// `body`. Any pairs in `extra_headers` are inserted before the two
/// helper-managed headers so callers can attach response-specific fields
/// such as `WWW-Authenticate`, or a `Connection: close` on a listener
/// that ends the connection after each response. Used for 4xx and 5xx
/// replies where the body is a short string the caller has already
/// assembled.
///
/// # Errors
///
/// Returns an `HttpError` if the connection state machine rejects the
/// response (for example, if `initiate_response` is called while the
/// connection is not in request state) or if the underlying socket
/// write fails.
///
/// # Panics
///
/// Panics if `extra_headers.len()` exceeds
/// `MAX_EXTRA_STATUS_HEADERS`. The bound exists so the helper can
/// assemble headers on the stack without allocation.
pub async fn write_status_only<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: u16,
    reason: &str,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    assert!(
        extra_headers.len() <= MAX_EXTRA_STATUS_HEADERS,
        "BUG: extra_headers exceeds MAX_EXTRA_STATUS_HEADERS"
    );

    let content_length = format_content_length(body.len());

    let mut headers: [(&str, &str); MAX_EXTRA_STATUS_HEADERS + 2] =
        [("", ""); MAX_EXTRA_STATUS_HEADERS + 2];
    let mut cursor = 0;
    for &header in extra_headers {
        headers[cursor] = header;
        cursor += 1;
    }
    headers[cursor] = ("Content-Type", content_type);
    cursor += 1;
    headers[cursor] = ("Content-Length", content_length.as_str());
    cursor += 1;

    connection
        .initiate_response(status, Some(reason), &headers[..cursor])
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Format `value` as an ASCII-decimal `Content-Length` string.
///
/// A 64-bit `usize` formats to at most `CONTENT_LENGTH_DECIMAL_DIGITS`
/// digits, so the result always fits the returned `String` and the
/// formatting write never fails.
#[must_use]
pub fn format_content_length(value: usize) -> String<CONTENT_LENGTH_DECIMAL_DIGITS> {
    let mut text = String::new();
    let _ = write!(text, "{value}");
    debug_assert_eq!(text.parse::<usize>(), Ok(value), "Content-Length truncated");
    text
}

/// Write a 200 JSON response with `body` bytes.
///
/// Every caller sends the same three headers, so there is no header
/// parameter; a response that needs more headers or another status
/// builds them at its own call site.
///
/// # Errors
///
/// Returns an `HttpError` if the connection state machine rejects the
/// response or if the underlying socket write fails.
pub async fn write_json_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    body: &[u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let content_length = format_content_length(body.len());

    let headers = [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
        ("Content-Length", content_length.as_str()),
    ];

    connection
        .initiate_response(200, Some("OK"), &headers)
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Read up to `buffer.len()` bytes of request body via the
/// `Connection::Read` impl.
///
/// Reads up to `content_length` bytes when provided, or until the
/// connection signals EOF otherwise. Returns the filled prefix of
/// `buffer` on success. Returns `None` when the declared
/// `content_length` exceeds the buffer or the read fails; the caller
/// should send a 413.
///
/// When `content_length` is absent (chunked or close-delimited
/// transfer), the read fills the buffer at most, then attempts a
/// one-byte overflow probe. If the probe reads a byte, the body
/// exceeded the buffer and the function returns `None` so the caller
/// can send a 413 rather than silently truncating the body.
pub async fn read_body<'buf, T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    buffer: &'buf mut [u8],
    content_length: Option<u64>,
) -> Option<&'buf [u8]>
where
    T: Read + Write + TcpSplit,
{
    let length_declared = match content_length {
        Some(declared) => {
            // On 32-bit targets a u64 Content-Length above usize::MAX
            // saturates to usize::MAX; that value still exceeds any
            // realistic buffer, so the body-too-large branch fires
            // unchanged and the response becomes a 413.
            let declared = usize::try_from(declared).unwrap_or(usize::MAX);
            if declared > buffer.len() {
                return None;
            }
            Some(declared)
        }
        None => None,
    };

    let want = length_declared.unwrap_or(buffer.len());
    let mut total = 0usize;
    while total < want {
        match embedded_io_async::Read::read(connection, &mut buffer[total..want]).await {
            Ok(0) => break,
            Ok(read) => total += read,
            Err(_) => return None,
        }
    }

    // When no Content-Length was declared, probe for one more byte. Any
    // returned byte means the body was larger than the buffer, so return
    // None to trigger a 413 at the call site. This prevents silent
    // truncation of chunked or close-delimited bodies.
    if length_declared.is_none() && total == buffer.len() {
        let mut overflow_probe = [0u8; 1];
        if let Ok(1) = embedded_io_async::Read::read(connection, &mut overflow_probe).await {
            return None;
        }
    }

    Some(&buffer[..total])
}
