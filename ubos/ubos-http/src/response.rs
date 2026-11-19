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
//! The helpers write directly to an edge-http `Connection`, filling
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

use crate::status::{Status, StatusMessage};

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

/// Start a response on `connection` with the status line of `status` and
/// `headers`.
///
/// Every response starts here, the writers below included, so the code
/// and the reason of a status line always come from one [`Status`].
///
/// # Errors
///
/// Returns an `HttpError` if the connection state machine rejects the
/// response or if the underlying socket write fails.
pub async fn start_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: Status,
    headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    connection
        .initiate_response(status.code(), Some(status.reason()), headers)
        .await
}

/// Write a complete HTTP status response to `connection`.
///
/// Sends the status line of `status`, and adds the
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
    status: Status,
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

    start_response(connection, status, &headers[..cursor]).await?;

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

/// Write a JSON response with `body` bytes.
///
/// `status` gives the status line. Every caller sends the
/// same three headers, so there is no header parameter; a response
/// that needs more headers builds them at its own call site.
///
/// # Errors
///
/// Returns an `HttpError` if the connection state machine rejects the
/// response or if the underlying socket write fails.
pub async fn write_json_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: Status,
    body: &[u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let content_length = format_content_length(body.len());

    start_response(connection, status, &json_headers(&content_length)).await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Write `response` as its status and the JSON body `{"message":"..."}`.
///
/// The body is written in pieces straight to the connection, so it needs
/// no buffer. A `"` or a `\` in the message is escaped and a control byte
/// is written as a space, so any text gives valid JSON.
///
/// # Errors
///
/// Returns an `HttpError` if the connection state machine rejects the
/// response or if the underlying socket write fails.
pub async fn write_message<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    response: StatusMessage,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = message_body(response.message());
    let content_length = format_content_length(body.clone().map(<[u8]>::len).sum());

    start_response(
        connection,
        response.status(),
        &json_headers(&content_length),
    )
    .await?;

    for piece in body {
        embedded_io_async::Write::write_all(connection, piece).await?;
    }

    Ok(())
}

/// The headers of a JSON response whose body is `content_length` bytes.
/// Both JSON writers send these three, so they are stated once.
fn json_headers(content_length: &str) -> [(&str, &str); 3] {
    [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
        ("Content-Length", content_length),
    ]
}

/// The pieces of the body `{"message":"..."}` for `message`, with each `"`
/// and `\` escaped and each control byte written as a space.
/// [`write_message`] sums their lengths for the `Content-Length` and then
/// writes them, so the two always agree.
pub(crate) fn message_body(message: &str) -> impl Iterator<Item = &[u8]> + Clone {
    let escaped = message
        .as_bytes()
        .split_inclusive(|&byte| json_escape(byte).is_some())
        .flat_map(|part| {
            let escaped = part
                .split_last()
                .and_then(|(&last, text)| Some([text, json_escape(last)?]));
            escaped.unwrap_or([part, b"".as_slice()])
        });
    core::iter::once(b"{\"message\":\"".as_slice())
        .chain(escaped)
        .chain(core::iter::once(b"\"}".as_slice()))
}

/// What stands for `byte` inside a JSON string, or `None` when the byte
/// stands as it is. JSON allows no raw `"`, `\` or control byte there. The
/// first two are escaped, and a control byte is written as a space, because
/// a message is text for a person and a space keeps it readable. A byte of
/// a multi-byte UTF-8 character is never one of them.
fn json_escape(byte: u8) -> Option<&'static [u8]> {
    match byte {
        b'"' => Some(b"\\\""),
        b'\\' => Some(b"\\\\"),
        _ if byte.is_ascii_control() => Some(b" "),
        _ => None,
    }
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
