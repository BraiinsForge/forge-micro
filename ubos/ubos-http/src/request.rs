//! Request header extraction helper.
//!
//! `Connection::headers()` returns a borrow that pins the connection,
//! so handlers cannot read the body or write a response without first
//! copying the fields they need into owned stack locals. Every listener
//! crate needs the same copy-out for method, path, and a fixed set of
//! header values, so the routine lives here.

use core::fmt::Write as _;

use edge_http::io::server::Connection;
use edge_http::io::Error as HttpError;
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};
use heapless::String;
use ubos_logging::warn;

/// Owned method, path, and selected header values for one request.
///
/// `origin` is the empty string when the `Origin` header is absent or
/// would not fit; handlers that do not implement CORS ignore it.
/// Oversize `Authorization`, `Cookie`, and `X-CSRF-Token` values are
/// reported as `None` so the route falls through to its anonymous or
/// unauthenticated branch.
pub struct RequestParts {
    pub method: String<16>,
    pub path: String<128>,
    pub authorization: Option<String<128>>,
    pub cookie: Option<String<256>>,
    pub csrf: Option<String<64>>,
    pub origin: String<128>,
    pub content_length: Option<u64>,
}

/// Copy the method, path, and tracked headers out of `connection`
/// into a `RequestParts` value that no longer borrows from it.
///
/// Path values longer than 128 bytes are left empty so the route
/// table falls through to a 404; oversize `Authorization`, `Cookie`,
/// and `Origin` values are dropped with a `warn!` log.
///
/// # Errors
///
/// Returns an `HttpError` when the connection state machine is not in
/// a state where headers can be read.
pub fn extract_request_parts<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<RequestParts, HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let headers = connection
        .headers()
        .map_err(|_| edge_http::io::Error::InvalidState)?;

    let mut method = String::<16>::new();
    let _ = core::write!(method, "{}", headers.method);

    let mut path = String::<128>::new();
    if headers.path.len() <= 128 {
        let _ = path.push_str(headers.path);
    }

    let authorization = headers.headers.get("Authorization").and_then(|value| {
        let mut owned = String::<128>::new();
        if owned.push_str(value).is_err() {
            warn!("Authorization header truncated, treating as missing");
            return None;
        }
        Some(owned)
    });

    let cookie = headers.headers.get("Cookie").and_then(|value| {
        let mut owned = String::<256>::new();
        if owned.push_str(value).is_err() {
            warn!("Cookie header truncated, treating as missing");
            return None;
        }
        Some(owned)
    });

    let csrf = headers.headers.get("X-CSRF-Token").and_then(|value| {
        let mut owned = String::<64>::new();
        owned.push_str(value).ok()?;
        Some(owned)
    });

    let content_length = headers.headers.content_len();

    let origin = headers
        .headers
        .get("Origin")
        .map_or_else(String::new, |value| {
            let mut owned = String::<128>::new();
            if owned.push_str(value).is_err() {
                warn!("Origin header truncated, treating as missing");
                return String::new();
            }
            owned
        });

    Ok(RequestParts {
        method,
        path,
        authorization,
        cookie,
        csrf,
        origin,
        content_length,
    })
}
