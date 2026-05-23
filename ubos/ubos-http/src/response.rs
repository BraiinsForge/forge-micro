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

#[cfg(test)]
#[path = "tests/response.rs"]
mod tests;
