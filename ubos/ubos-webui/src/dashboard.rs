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

//! Dashboard service and HTTP handler for the firmware web UI.
//!
//! The transport loop and request parsing live in `ubos-http`, and the
//! JSON `/api/*` surface in `ubos-api` behind the router that shares the
//! listener. This module owns the handler dispatch and the response
//! streaming through `edge-http`'s `Connection` state machine.
//!
//! - `GET /` returns the HTML dashboard, which is fully static: every
//!   time-varying field is filled in client-side from the API surface.
//! - `HEAD /` mirrors it with no body, which is how the page refreshes
//!   its single-use CSRF token without downloading the HTML again.
//! - `GET /login`, `POST /login` and `POST /logout` drive the session
//!   cookie the page carries to the API surface.
//! - `GET /logo.svg` stays a separate cacheable asset so the index
//!   response fits its buffer. The tab icon goes the other way and is
//!   inlined into each page, for the reason `build.rs` records on
//!   `icon_data_uri`.
//!
//! Any other path returns 404, and a wrong method on a known path 405.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};
use heapless::Vec;

use crate::{INDEX_HTML_GZ, LOGIN_HTML_GZ, LOGO_SVG_GZ};
use ubos_http::consts::Http;
use ubos_http::{
    MAX_RESPONSE_HEADERS, RequestParts, Service, Status, format_content_length, read_body,
    start_response, write_message, write_status_only,
};

#[cfg(test)]
#[path = "tests/dashboard.rs"]
mod tests;

/// Buffer for reading request bodies. A body that does not fit is
/// answered with 413.
const BODY_READ_BUFFER_SIZE: usize = Http::REQUEST_BUFFER_SIZE;

async fn write_redirect<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    location: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    start_response(
        connection,
        Status::FOUND,
        &[("Location", location), ("Content-Length", "0")],
    )
    .await
}

async fn write_logout_redirect<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    start_response(
        connection,
        Status::FOUND,
        &[
            ("Location", "/login"),
            (
                "Set-Cookie",
                "session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            ),
            ("Content-Length", "0"),
        ],
    )
    .await
}

async fn write_login_success_redirect<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    cookie_header: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    start_response(
        connection,
        Status::FOUND,
        &[
            ("Location", "/"),
            ("Set-Cookie", cookie_header),
            ("Content-Length", "0"),
        ],
    )
    .await
}

/// Write an empty `200 OK` for `HEAD /`.
///
/// Content-Encoding is omitted so that edge-http's `SendBody::finish`
/// accepts the zero bytes that follow `initiate_response`.
async fn write_head_ok<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut headers: Vec<(&str, &str), MAX_RESPONSE_HEADERS> = Vec::new();
    // The pushes are infallible while the caller's extras keep the total
    // within MAX_RESPONSE_HEADERS. No response sends a Connection header,
    // so HTTP/1.1 keeps the connection alive for the next request.
    headers
        .push(("Content-Type", "text/html; charset=utf-8"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", "0"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Cache-Control", "no-store"))
        .expect("BUG: response header vec sized for max headers");
    for &header in extra_headers {
        headers
            .push(header)
            .expect("BUG: response header vec sized for max headers");
    }
    start_response(connection, Status::OK, &headers).await
}

/// Write a static asset response, `encoding` naming the
/// `Content-Encoding` a compressed asset needs.
async fn write_asset<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    content_type: &str,
    cache_control: &str,
    encoding: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let content_length = format_content_length(body.len());

    // The pushes are infallible: three fixed headers, one optional
    // encoding and at most two extras stay within MAX_RESPONSE_HEADERS.
    let mut headers: Vec<(&str, &str), MAX_RESPONSE_HEADERS> = Vec::new();
    headers
        .push(("Content-Type", content_type))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Cache-Control", cache_control))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", content_length.as_str()))
        .expect("BUG: response header vec sized for max headers");
    if let Some(enc) = encoding {
        headers
            .push(("Content-Encoding", enc))
            .expect("BUG: response header vec sized for max headers");
    }
    for &header in extra_headers {
        headers
            .push(header)
            .expect("BUG: response header vec sized for max headers");
    }

    start_response(connection, Status::OK, &headers).await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Decoded form-field length cap. Larger inputs are rejected.
const FORM_FIELD_MAX_LEN: usize = 64;

/// Extract a URL-encoded form field value from a `key=value&...` body,
/// percent-decoded.
///
/// Returns the decoded value, or `None` when the field is missing, the
/// decoded value exceeds [`FORM_FIELD_MAX_LEN`], or the value contains
/// malformed `%`-escapes or non-UTF-8 bytes. `+` is decoded as a space
/// per `application/x-www-form-urlencoded`.
fn form_field(body: &str, key: &str) -> Option<heapless::String<FORM_FIELD_MAX_LEN>> {
    let mut prefix = heapless::String::<64>::new();
    prefix.push_str(key).ok()?;
    prefix.push('=').ok()?;
    let prefix = prefix.as_str();

    // A key matches only at the start of the body or after a '&'.
    let start = if body.starts_with(prefix) {
        prefix.len()
    } else {
        let needle_with_amp = {
            let mut s = heapless::String::<65>::new();
            s.push('&').ok()?;
            s.push_str(prefix).ok()?;
            s
        };
        let pos = body.find(needle_with_amp.as_str())?;
        pos + needle_with_amp.len()
    };

    let value_and_rest = &body[start..];
    let end = value_and_rest.find('&').unwrap_or(value_and_rest.len());
    percent_decode(&value_and_rest[..end])
}

/// Decode a `application/x-www-form-urlencoded` value into a bounded
/// string. Returns `None` on malformed `%`-escapes, non-UTF-8 output, or
/// overflow past [`FORM_FIELD_MAX_LEN`].
fn percent_decode(input: &str) -> Option<heapless::String<FORM_FIELD_MAX_LEN>> {
    let mut buffer = [0u8; FORM_FIELD_MAX_LEN];
    let mut written = 0usize;
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if written >= buffer.len() {
            return None;
        }
        let byte = bytes[index];
        let decoded = match byte {
            b'+' => {
                index += 1;
                b' '
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = (bytes[index + 1] as char).to_digit(16)?;
                let low = (bytes[index + 2] as char).to_digit(16)?;
                index += 3;
                #[allow(clippy::cast_possible_truncation)]
                {
                    ((high << 4) | low) as u8
                }
            }
            other => {
                index += 1;
                other
            }
        };
        buffer[written] = decoded;
        written += 1;
    }
    let text = core::str::from_utf8(&buffer[..written]).ok()?;
    let mut result = heapless::String::<FORM_FIELD_MAX_LEN>::new();
    result.push_str(text).ok()?;
    Some(result)
}

/// Dashboard handler implementing `ubos_http::Service` over the web-UI
/// shell: the page, the login form, logout and the logo.
pub struct DashboardService {
    net: ubos_net::net::Handle,
}

impl DashboardService {
    /// Serve the web UI and count each request on `net`.
    #[must_use]
    pub const fn new(net: ubos_net::net::Handle) -> Self {
        Self { net }
    }
}

impl Service for DashboardService {
    // The length is a flat match dispatch table rather than nesting, and
    // splitting it would thread the generic `Connection` through every
    // helper for no gain.
    #[allow(clippy::too_many_lines)]
    async fn handle<TcpSocket, const N: usize>(
        &self,
        peer_ip: core::net::IpAddr,
        parts: &RequestParts,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit,
    {
        // The count is of handled requests, not of successful ones: the
        // metrics port does not increment it, and a connection dropped
        // before its request is parsed is never counted.
        self.net.record_request();

        let method = parts.method.as_str();
        let path = parts.path.as_str();
        let authorization = parts.authorization.as_deref();
        let cookie = parts.cookie.as_deref();
        let content_length = parts.content_length;

        // Anonymous requests are admitted because an unauthenticated
        // browser must still reach `/login`.
        let identity_policy = ubos_auth::Policy {
            allow_anonymous: true,
        };
        // With anonymous requests admitted, the only rejection left is the
        // allow-list, whichever variant names it.
        let Ok(auth_ctx) =
            ubos_auth::gate_identity(peer_ip, authorization, cookie, &identity_policy)
        else {
            return write_message(
                connection,
                Status::UNAUTHORIZED.with_message("Client not in allowed network range"),
            )
            .await;
        };

        let is_mutating = matches!(method, "POST" | "PATCH" | "DELETE");

        // The body is drained here so that edge-http reaches the
        // response-write state before `initiate_response`, and into a
        // separate stack buffer, because the work buffer is reused for the
        // response.
        //
        // It is read before the CSRF check so that an oversized payload
        // answered with 413 does not consume the single-use token; the
        // client can retry the trimmed body with the same token.
        let needs_body = is_mutating;

        let mut body_buffer = [0u8; BODY_READ_BUFFER_SIZE];
        let body: &[u8] = if needs_body {
            match read_body(connection, &mut body_buffer, content_length).await {
                Some(bytes) => bytes,
                None => {
                    return write_status_only(
                        connection,
                        Status::PAYLOAD_TOO_LARGE,
                        "text/plain",
                        &[],
                        b"payload too large",
                    )
                    .await;
                }
            }
        } else {
            &[]
        };

        // Without this gate an attacker page could log a victim browser
        // into the attacker's own account and then read whatever the
        // victim puts in that session. It binds to the session already
        // presented, so a fresh anonymous login is not gated and a
        // Basic-authenticated request, which carries no session, is exempt.
        //
        // The identity is the one resolved above, so the body read between
        // the two cannot change who is being checked.
        let needs_csrf = path == "/login" && is_mutating;
        if needs_csrf && ubos_auth::check_csrf(&auth_ctx, parts.csrf.as_deref()).is_err() {
            return write_message(
                connection,
                Status::FORBIDDEN.with_message("CSRF token invalid or missing"),
            )
            .await;
        }

        match (method, path) {
            ("GET", "/") => {
                match auth_ctx {
                    ubos_auth::auth::AuthCtx::Anonymous => {
                        write_redirect(connection, "/login").await
                    }
                    ubos_auth::auth::AuthCtx::Session(ref session_id) => {
                        let csrf_token = ubos_auth::csrf::issue_csrf_token(session_id.as_str());
                        let csrf_header_value = csrf_token.as_str();
                        write_asset(
                            connection,
                            "text/html; charset=utf-8",
                            "no-store",
                            Some("gzip"),
                            &[("X-CSRF-Token", csrf_header_value)],
                            INDEX_HTML_GZ,
                        )
                        .await
                    }
                    // Basic clients do not use the SPA's CSRF surface.
                    ubos_auth::auth::AuthCtx::Basic => {
                        write_asset(
                            connection,
                            "text/html; charset=utf-8",
                            "no-store",
                            Some("gzip"),
                            &[],
                            INDEX_HTML_GZ,
                        )
                        .await
                    }
                }
            }
            ("HEAD", "/") => match auth_ctx {
                ubos_auth::auth::AuthCtx::Anonymous => write_redirect(connection, "/login").await,
                ubos_auth::auth::AuthCtx::Session(ref session_id) => {
                    let csrf_token = ubos_auth::csrf::issue_csrf_token(session_id.as_str());
                    write_head_ok(connection, &[("X-CSRF-Token", csrf_token.as_str())]).await
                }
                ubos_auth::auth::AuthCtx::Basic => write_head_ok(connection, &[]).await,
            },
            ("GET", "/login") => {
                // A live session has nothing to sign in to, and the form
                // it would post is CSRF-gated with no way to carry the
                // token. Send it to the dashboard instead.
                if matches!(auth_ctx, ubos_auth::auth::AuthCtx::Session(_)) {
                    return write_redirect(connection, "/").await;
                }
                write_asset(
                    connection,
                    "text/html; charset=utf-8",
                    "no-store",
                    Some("gzip"),
                    &[],
                    LOGIN_HTML_GZ,
                )
                .await
            }
            ("POST", "/login") => {
                // An empty string stands in for a missing or malformed
                // field, so the constant-time compare below still runs.
                let body_str = core::str::from_utf8(body).unwrap_or("");
                let supplied_username = form_field(body_str, "username").unwrap_or_default();
                let supplied_password = form_field(body_str, "password").unwrap_or_default();

                let stored = ubos_auth::auth::current_credentials();
                let username_ok = ubos_auth::auth::constant_time_eq(
                    stored.username.as_str(),
                    supplied_username.as_str(),
                );
                let password_ok = ubos_auth::auth::constant_time_eq(
                    stored.password.as_str(),
                    supplied_password.as_str(),
                );
                // Bitwise `&` (not `&&`) so the branch timing does not
                // depend on which field mismatched first.
                if username_ok & password_ok {
                    // A stale cookie from a prior login must not outlive
                    // the login happening now.
                    let existing_session = ubos_auth::auth::extract_session_cookie_from_str(cookie);
                    if let Some(existing) = existing_session {
                        ubos_auth::session::invalidate_session(existing);
                    }
                    let token = ubos_auth::csprng::random_token();
                    ubos_auth::session::create_session(token.clone());
                    // `Secure` is omitted because the device runs HTTP
                    // only, with no TLS terminator. `SameSite=Strict`
                    // would withhold the cookie on a link from another
                    // site, bouncing a live session to a login page whose
                    // CSRF-gated form has no way to pass, so `Lax` sends
                    // it on a top-level navigation and still withholds it
                    // on a cross-site POST or fetch.
                    let mut set_cookie_value = heapless::String::<128>::new();
                    let _ = core::write!(
                        set_cookie_value,
                        "session={}; Path=/; HttpOnly; SameSite=Lax",
                        token.as_str()
                    );
                    write_login_success_redirect(connection, set_cookie_value.as_str()).await
                } else {
                    write_redirect(connection, "/login?err=1").await
                }
            }
            ("POST", "/logout") => {
                // Logout is not gated on auth: a navigation form cannot
                // carry the CSRF header, and answering an anonymous logout
                // with 401 plus `WWW-Authenticate: Basic` would raise the
                // browser's native credential prompt instead of the login
                // page. Invalidating a session that is not there is a
                // no-op.
                if let Some(token) = ubos_auth::auth::extract_session_cookie_from_str(cookie) {
                    ubos_auth::session::invalidate_session(token);
                }
                write_logout_redirect(connection).await
            }
            ("GET", "/logo.svg") => {
                write_asset(
                    connection,
                    "image/svg+xml",
                    "public, max-age=86400",
                    Some("gzip"),
                    &[],
                    LOGO_SVG_GZ,
                )
                .await
            }
            (_, "/" | "/login" | "/logout" | "/logo.svg") => {
                write_status_only(
                    connection,
                    Status::METHOD_NOT_ALLOWED,
                    "text/plain",
                    &[],
                    b"method not allowed",
                )
                .await
            }
            _ => {
                write_status_only(
                    connection,
                    Status::NOT_FOUND,
                    "text/plain",
                    &[],
                    b"not found",
                )
                .await
            }
        }
    }
}

/// Seed the credential store from the persisted config.
///
/// Call once before spawning the web-UI workers, so the first request
/// authenticates against the operator's configured credentials. The REST
/// API listener owns the config watch that keeps the store in step after
/// that.
pub async fn seed_webui_credentials(config: ubos_config::Handle) {
    let api = config.current().await.api;
    ubos_auth::auth::set_credentials(&api.username, &api.password);
    ubos_auth::auth::warn_if_default_credentials();
}
