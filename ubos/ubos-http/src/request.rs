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

//! Request header extraction. `Connection::headers()` borrows the
//! connection, so a handler copies out what it needs before it reads
//! the body or writes.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};
use heapless::String;
use ubos_logging::warn;

/// Owned method, path and tracked header values for one request. An
/// oversize credential header reads as `None`, so the route falls
/// through to its unauthenticated branch.
pub struct RequestParts {
    pub method: String<16>,
    pub path: String<128>,
    pub authorization: Option<String<128>>,
    pub cookie: Option<String<256>>,
    pub csrf: Option<String<64>>,
    pub content_length: Option<u64>,
    /// `X-OTA-Offset`, read only by the OTA chunk upload.
    pub ota_offset: Option<String<20>>,
    /// `X-OTA-Final`, read only by the OTA chunk upload.
    pub ota_final: Option<String<4>>,
}

/// Copy the parts out of `connection`. An oversize path is left empty so
/// the route table answers 404.
///
/// # Errors
///
/// When the connection is not in the headers state.
pub fn extract_request_parts<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<RequestParts, HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let headers = connection
        .headers()
        .map_err(|_| edge_http::io::Error::InvalidState)?;

    // `Method::as_str` is private upstream; see
    // `docs/upstream-prs/edge-http-method-as-str-public.md`.
    let mut method = String::<16>::new();
    let _ = core::write!(method, "{}", headers.method);

    let mut path = String::<128>::new();
    if headers.path.len() <= 128 {
        let _ = path.push_str(headers.path);
    } else {
        warn!("request path truncated, treating as empty");
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

    // `Connection::new` rejects a `Content-Length` that is not a length
    // and the worker answers 400, so the header parses here.
    let content_length = headers.headers.content_len()?;

    let ota_offset = headers.headers.get("X-OTA-Offset").and_then(|value| {
        let mut owned = String::<20>::new();
        owned.push_str(value).ok()?;
        Some(owned)
    });

    let ota_final = headers.headers.get("X-OTA-Final").and_then(|value| {
        let mut owned = String::<4>::new();
        owned.push_str(value).ok()?;
        Some(owned)
    });

    Ok(RequestParts {
        method,
        path,
        authorization,
        cookie,
        csrf,
        content_length,
        ota_offset,
        ota_final,
    })
}
