//! Dashboard service and HTTP handler for the firmware web UI.
//!
//! Owns only the web-UI surface on port 80; the JSON `/api/*` surface
//! moved to the REST listener in `crate::rest` on port 8080.
//!
//! - `GET /` returns the inlined HTML dashboard. The page is fully
//!   static — every time-varying field is filled in client-side via
//!   cross-origin fetches against the REST listener. Hash-routed SPA
//!   with a top nav bar, three single-axis charts for hashrate,
//!   temperature, and input/output power, a right-side telemetry
//!   panel led by a Link status card, and a configuration form view.
//! - `HEAD /` mirrors `GET /` but returns no body. The dashboard
//!   uses it to refresh the single-use CSRF token between mutating
//!   requests without re-downloading the gzipped HTML asset.
//! - `GET /login`, `POST /login`, `POST /logout` drive the session
//!   cookie used by the SPA when it calls the REST listener.
//! - `GET /logo.svg` serves the Braiins wordmark referenced by the
//!   dashboard's nav bar. Kept as a separate cacheable asset rather
//!   than inlined so the index response stays inside its buffer.
//!
//! Any other path returns a 404 with a plain-text body. Wrong-method
//! requests on known paths return 405. The transport loop and request
//! parsing live in `ubos-http`; this module owns the handler dispatch
//! and the response streaming through `edge-http`'s `Connection` state
//! machine.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};
use heapless::Vec;

use crate::consts::Http;
use crate::{INDEX_HTML_GZ, LOGIN_HTML_GZ, LOGO_SVG};
use ubos_http::{
    MAX_RESPONSE_HEADERS, RequestParts, Service, extract_request_parts, format_content_length,
    read_body, write_status_only,
};

#[cfg(test)]
#[path = "tests/dashboard.rs"]
mod tests;

/// Buffer for reading POST/PATCH request bodies.
///
/// Sized to `Http::REQUEST_BUFFER_SIZE` so a login form or config
/// payload fits in one allocation. Requests that exceed this cap
/// receive a 413 response.
const BODY_READ_BUFFER_SIZE: usize = Http::REQUEST_BUFFER_SIZE;

/// Write a 302 redirect response to `location`.
async fn write_redirect<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    location: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    connection
        .initiate_response(
            302,
            Some("Found"),
            &[("Location", location), ("Content-Length", "0")],
        )
        .await
}

/// Write a 302 redirect that also clears the session cookie.
async fn write_logout_redirect<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    connection
        .initiate_response(
            302,
            Some("Found"),
            &[
                ("Location", "/login"),
                (
                    "Set-Cookie",
                    "session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
                ),
                ("Content-Length", "0"),
            ],
        )
        .await
}

/// Write a 302 redirect that sets the session cookie to `token`.
async fn write_login_success_redirect<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    cookie_header: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    connection
        .initiate_response(
            302,
            Some("Found"),
            &[
                ("Location", "/"),
                ("Set-Cookie", cookie_header),
                ("Content-Length", "0"),
            ],
        )
        .await
}

/// Write an empty `200 OK` body-less response for `HEAD /`.
///
/// `extra_headers` carries the optional `X-CSRF-Token` for session
/// clients. Content-Length is fixed at 0 and Content-Encoding is
/// intentionally omitted so edge-http's `SendBody::finish` accepts the
/// zero bytes that follow `initiate_response`.
async fn write_head_ok<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut headers: Vec<(&str, &str), MAX_RESPONSE_HEADERS> = Vec::new();
    // Three fixed slots plus caller-supplied extras. The .expect() calls
    // assume the call site stays within MAX_RESPONSE_HEADERS=8. No
    // Connection header is sent, so HTTP/1.1 keeps the connection alive for
    // the next request.
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
    connection
        .initiate_response(200, Some("OK"), &headers)
        .await
}

/// Write a static asset response (gzipped HTML or SVG) to `connection`.
///
/// `content_type` is the MIME type. `cache_control` is the header value.
/// `encoding` is the optional `Content-Encoding` value; pass `Some("gzip")`
/// for gzip assets and `None` for plain assets.
/// `extra_headers` may carry additional headers such as `X-CSRF-Token`.
/// `body` is the raw asset bytes.
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

    // Assemble headers on the stack. The capacity is
    // MAX_RESPONSE_HEADERS; each push is infallible within that budget
    // because the slots used here are counted statically: 3 fixed plus
    // 1 optional encoding plus extra_headers up to 2, totalling at most
    // 6, within the 8-slot cap. No Connection header is sent, so HTTP/1.1
    // keeps the connection alive for the next asset request.
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

    connection
        .initiate_response(200, Some("OK"), &headers)
        .await?;

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
    // Build "key=" prefix to search for.
    let mut prefix = heapless::String::<64>::new();
    prefix.push_str(key).ok()?;
    prefix.push('=').ok()?;
    let prefix = prefix.as_str();

    // Search for exact key match at start or after a '&'.
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

/// Dashboard handler implementing `ubos_http::Service`.
///
/// Serves the web-UI shell on port 80 (HTML, login, logout, logo).
/// Methods are enforced: wrong-method requests on known paths return
/// 405. Every request that reaches this handler increments the request
/// counter via `ubos_net::net::handle().record_request` regardless of
/// response status, so the metric reflects handled-request volume rather
/// than success rate.
pub struct DashboardService;

impl Service for DashboardService {
    // The line count comes from the flat match dispatch table, not
    // from deep nesting. Splitting the table into per-path helpers
    // would require threading the generic `Connection` parameter through
    // each helper function, adding noise without reducing complexity.
    #[allow(clippy::too_many_lines)]
    async fn handle<TcpSocket, const N: usize>(
        &self,
        peer_ip: core::net::IpAddr,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit,
    {
        // Count each request that reaches this handler so
        // `board_http_requests_total` reflects dashboard and API traffic.
        // The metrics port does not increment it, and a connection
        // dropped before its request is parsed is never counted.
        ubos_net::net::handle().record_request();

        // The allow-list is the compile-time default for now.
        if !ubos_auth::allow_list::is_allowed(peer_ip, ubos_auth::allow_list::DEFAULT_ALLOW_LIST) {
            return write_status_only(
                connection,
                401,
                "Unauthorized",
                "application/json",
                &[],
                b"{\"message\":\"Client not in allowed network range\"}",
            )
            .await;
        }

        let RequestParts {
            method: method_buf,
            path: path_buf,
            authorization: authorization_buf,
            cookie: cookie_buf,
            csrf: csrf_buf,
            content_length,
            ..
        } = extract_request_parts(connection)?;

        let method = method_buf.as_str();
        let path = path_buf.as_str();
        let authorization = authorization_buf.as_deref();
        let cookie = cookie_buf.as_deref();

        let auth_ctx = ubos_auth::auth::authenticate(authorization, cookie)
            .await
            .unwrap_or(ubos_auth::auth::AuthCtx::Anonymous);

        // Logout is idempotent and unauthenticated. The logout control is
        // a navigation form, so a top-level POST that arrives without a
        // live session must still land on `/login` by following the
        // redirect. Gating it on auth would answer an anonymous logout
        // with `401` plus `WWW-Authenticate: Basic`, which the browser
        // renders as a native credential prompt instead. That happens
        // whenever the session is gone but the tab still shows the button,
        // for example after the in-RAM sessions are cleared by a reboot.
        // The handler invalidates whatever session the cookie names, which
        // is a no-op when there is none, then redirects.

        let is_mutating = matches!(method, "POST" | "PATCH" | "DELETE");

        // For any mutating request, drain the body here so handlers
        // have the bytes ready and edge-http reaches the response-write
        // state before `initiate_response` is called. The work buffer is
        // reused for the response after `initiate_response`, so the body
        // must be in a separate stack buffer.
        //
        // The body is read before the CSRF check so that an oversized
        // payload returning 413 does not consume the single-use CSRF
        // token; the client can retry with a trimmed body using the same
        // token instead of refetching `GET /`.
        let needs_body = is_mutating;

        let mut body_buffer = [0u8; BODY_READ_BUFFER_SIZE];
        let body: &[u8] = if needs_body {
            match read_body(connection, &mut body_buffer, content_length).await {
                Some(bytes) => bytes,
                None => {
                    return write_status_only(
                        connection,
                        413,
                        "Payload Too Large",
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

        // POST /login requires a valid X-CSRF-Token header issued for the
        // presented session. A victim browser on the LAN could otherwise
        // be force-logged-in by an attacker page: the attacker submits
        // their own credentials into the victim's browser, takes over the
        // active session, and then observes any state the victim stores in
        // that session. The check applies only when a session cookie is
        // already present so that a fresh anonymous login (no prior
        // session) is not gated; the token is issued to the existing
        // session on `GET /` and reused here. Basic-authenticated requests
        // are exempt because they carry no SPA session.
        //
        // POST /logout is intentionally not CSRF-gated. Logout is a plain
        // navigation form so the browser follows the redirect to /login
        // exactly like the login form; a form POST cannot send the
        // X-CSRF-Token header, and a forced logout is a low-value CSRF
        // target that exposes no state and changes no credentials.
        let needs_csrf = path == "/login" && is_mutating;
        if needs_csrf && let ubos_auth::auth::AuthCtx::Session(ref session_id) = auth_ctx {
            let token_str = csrf_buf.as_deref().unwrap_or("");
            if token_str.is_empty()
                || !ubos_auth::csrf::consume_csrf_token(session_id.as_str(), token_str).await
            {
                return write_status_only(
                    connection,
                    403,
                    "Forbidden",
                    "application/json",
                    &[],
                    b"{\"message\":\"CSRF token invalid or missing\"}",
                )
                .await;
            }
        }

        match (method, path) {
            ("GET", "/") => {
                match auth_ctx {
                    ubos_auth::auth::AuthCtx::Anonymous => {
                        write_redirect(connection, "/login").await
                    }
                    ubos_auth::auth::AuthCtx::Session(ref session_id) => {
                        let csrf_token =
                            ubos_auth::csrf::issue_csrf_token(session_id.as_str()).await;
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
            ("HEAD", "/") => {
                // Issues a fresh CSRF token in the response headers
                // without serving the dashboard body, so the SPA can
                // refresh its token between mutating requests without
                // refetching the gzipped HTML asset. Content-Length is
                // 0 and Content-Encoding is omitted because edge-http
                // finishes the response by checking advertised length
                // against bytes written, and the client only consumes
                // the response headers anyway.
                match auth_ctx {
                    ubos_auth::auth::AuthCtx::Anonymous => {
                        write_redirect(connection, "/login").await
                    }
                    ubos_auth::auth::AuthCtx::Session(ref session_id) => {
                        let csrf_token =
                            ubos_auth::csrf::issue_csrf_token(session_id.as_str()).await;
                        write_head_ok(connection, &[("X-CSRF-Token", csrf_token.as_str())]).await
                    }
                    ubos_auth::auth::AuthCtx::Basic => write_head_ok(connection, &[]).await,
                }
            }
            ("GET", "/login") => {
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
                // Parse `username=...&password=...` form body. Empty
                // strings substitute for missing or malformed fields so
                // the constant-time compare below still runs.
                let body_str = core::str::from_utf8(body).unwrap_or("");
                let supplied_username = form_field(body_str, "username").unwrap_or_default();
                let supplied_password = form_field(body_str, "password").unwrap_or_default();

                let stored = ubos_auth::auth::current_credentials().await;
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
                    // Invalidate any session presented on the inbound
                    // request so a stale cookie from a prior login cannot
                    // outlive the current login.
                    let existing_session = ubos_auth::auth::extract_session_cookie_from_str(cookie);
                    if let Some(existing) = existing_session {
                        ubos_auth::session::invalidate_session(existing).await;
                    }
                    let token = ubos_auth::csprng::random_token().await;
                    ubos_auth::session::create_session(token.clone()).await;
                    // Build the Set-Cookie header value into a stack buffer.
                    // The `Secure` attribute is omitted because the device
                    // runs HTTP only with no TLS terminator; revisit if
                    // HTTPS support is added.
                    let mut set_cookie_value = heapless::String::<128>::new();
                    let _ = core::write!(
                        set_cookie_value,
                        "session={}; Path=/; HttpOnly; SameSite=Strict",
                        token.as_str()
                    );
                    write_login_success_redirect(connection, set_cookie_value.as_str()).await
                } else {
                    write_redirect(connection, "/login?err=1").await
                }
            }
            ("POST", "/logout") => {
                if let Some(token) = ubos_auth::auth::extract_session_cookie_from_str(cookie) {
                    ubos_auth::session::invalidate_session(token).await;
                }
                write_logout_redirect(connection).await
            }
            ("GET", "/logo.svg") => {
                write_asset(
                    connection,
                    "image/svg+xml",
                    "public, max-age=86400",
                    None,
                    &[],
                    LOGO_SVG,
                )
                .await
            }
            (_, "/" | "/login" | "/logout" | "/logo.svg") => {
                write_status_only(
                    connection,
                    405,
                    "Method Not Allowed",
                    "text/plain",
                    &[],
                    b"method not allowed",
                )
                .await
            }
            _ => {
                write_status_only(
                    connection,
                    404,
                    "Not Found",
                    "text/plain",
                    &[],
                    b"not found",
                )
                .await
            }
        }
    }
}

/// Seed the credential store from the persisted config before the accept loop.
///
/// Call once before spawning web-UI worker tasks so the first request
/// authenticates against the operator's configured credentials. The REST
/// API listener owns the config watch that keeps the shared credential
/// store in sync on later changes.
pub async fn seed_webui_credentials() {
    let config = ubos_config::handle();
    let api = config.current().api;
    ubos_auth::auth::set_credentials(&api.username, &api.password).await;
    ubos_auth::auth::warn_if_default_credentials().await;
}
