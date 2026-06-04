//! Shared HTTP response helpers for edge-http connections.
//!
//! Both helpers write directly to an edge-http `Connection`, filling
//! the gap in the edge-http API: the library ships no numeric
//! `Content-Length` header helper and no shared error-response writer.
//! Handler crates import these to avoid duplicating the same header
//! assembly logic.

use edge_http::io::server::Connection;
use edge_http::io::Error as HttpError;
use embedded_io_async::{Read, Write};
use heapless::Vec;

use edge_nal::TcpSplit;

/// Maximum number of ASCII decimal digits needed for any `usize` value.
///
/// A 64-bit `usize` has at most 20 decimal digits. All targets in this
/// workspace are 32-bit, but the constant is sized for the widest
/// possible `usize` so the function is safe on any target without
/// revision.
pub const CONTENT_LENGTH_DECIMAL_DIGITS: usize = 20;

/// Maximum number of extra headers a caller may attach to a status
/// response on top of the three the helper writes itself
/// (`Content-Type`, `Content-Length`, `Connection`).
pub const MAX_EXTRA_STATUS_HEADERS: usize = 4;

/// Maximum number of response headers built per request.
///
/// Most responses use 4 headers (Content-Type, Cache-Control,
/// Connection, Content-Length). Session responses need an additional
/// Set-Cookie or X-CSRF-Token header. Eight slots cover the worst-case
/// header count (4 fixed + 1 encoding + 1 extra = 6) with two slots to
/// spare.
pub const MAX_RESPONSE_HEADERS: usize = 8;

/// Write a complete HTTP status response to `connection`.
///
/// Sends `status` and `reason` as the status line, adds the
/// caller-provided `content_type`, a `Content-Length` derived from
/// `body`, and `Connection: close`. Any pairs in `extra_headers` are
/// inserted before the three helper-managed headers so callers can
/// attach response-specific fields such as `WWW-Authenticate`. Used
/// for 4xx and 5xx replies where the body is a short string the
/// caller has already assembled.
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

    let mut length_buffer = [0u8; CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = format_content_length(&mut length_buffer, body.len());

    let mut headers: [(&str, &str); MAX_EXTRA_STATUS_HEADERS + 3] =
        [("", ""); MAX_EXTRA_STATUS_HEADERS + 3];
    let mut cursor = 0;
    for &header in extra_headers {
        headers[cursor] = header;
        cursor += 1;
    }
    headers[cursor] = ("Content-Type", content_type);
    cursor += 1;
    headers[cursor] = ("Content-Length", content_length);
    cursor += 1;
    headers[cursor] = ("Connection", "close");
    cursor += 1;

    connection
        .initiate_response(status, Some(reason), &headers[..cursor])
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Format `value` as ASCII decimal into `buffer` and return a `&str`
/// slice of the written bytes.
///
/// The returned slice is borrowed from `buffer` and is valid for as
/// long as the borrow on `buffer` lasts. `N` must be at least
/// `CONTENT_LENGTH_DECIMAL_DIGITS` (20) to hold any possible `usize`;
/// passing a smaller buffer is a caller error. If a value requires
/// more decimal digits than `N`, the function panics on cursor
/// underflow rather than producing a truncated string.
///
/// # Panics
///
/// Panics when `N` is smaller than the number of decimal digits
/// needed to format `value`; the right-to-left cursor underflows.
/// Callers must size the buffer with `CONTENT_LENGTH_DECIMAL_DIGITS`
/// or a value derived from `value`'s upper bound.
pub fn format_content_length<const N: usize>(buffer: &mut [u8; N], value: usize) -> &str {
    // Write the decimal digits right-to-left into the tail of the
    // buffer, then return a borrow of just that tail. The most
    // significant digit naturally ends up at the lowest written index.
    let mut remaining = value;
    let mut cursor = N;

    loop {
        // Each iteration places one digit. The loop always executes at
        // least once so that zero formats as "0".
        cursor -= 1;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "remainder of division by 10 is always in 0..=9"
        )]
        let digit = (remaining % 10) as u8;
        buffer[cursor] = b'0' + digit;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }

    let slice = &buffer[cursor..];

    // Safety: the buffer contains only ASCII digit bytes written above.
    #[expect(
        clippy::expect_used,
        reason = "BUG: buffer contains only ASCII digit bytes; UTF-8 validity is guaranteed"
    )]
    core::str::from_utf8(slice)
        // BUG: slice contains only b'0'..=b'9', which are valid UTF-8
        .expect("BUG: digit bytes are valid UTF-8")
}

/// Write a JSON response with `body` bytes and optional extra headers.
///
/// The REST listener attaches CORS headers via `extra_headers`; pass
/// `&[]` to send only the four fixed headers. The total must not
/// exceed `MAX_RESPONSE_HEADERS`.
///
/// # Errors
///
/// Returns an `HttpError` if the connection state machine rejects the
/// response or if the underlying socket write fails.
///
/// # Panics
///
/// Panics if `extra_headers.len()` would push the header count above
/// `MAX_RESPONSE_HEADERS`. The four fixed headers (Content-Type,
/// Cache-Control, Connection, Content-Length) are always emitted, so
/// `extra_headers` must contain at most `MAX_RESPONSE_HEADERS - 4`
/// entries.
pub async fn write_json_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut length_buffer = [0u8; CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = format_content_length(&mut length_buffer, body.len());

    let mut headers: Vec<(&str, &str), MAX_RESPONSE_HEADERS> = Vec::new();
    headers
        .push(("Content-Type", "application/json"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Cache-Control", "no-store"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Connection", "close"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", content_length))
        .expect("BUG: response header vec sized for max headers");
    for &header in extra_headers {
        headers
            .push(header)
            .expect("BUG: response header vec sized for max headers");
    }

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

#[cfg(test)]
#[path = "tests/response.rs"]
mod tests;
