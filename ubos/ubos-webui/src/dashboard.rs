//! Dashboard service and HTTP handler for the firmware web UI.
//!
//! Owns only the web-UI surface on port 80; the JSON `/api/*` surface
//! moved to the REST listener in `crate::rest` on port 8080.
//!
//! - `GET /` returns the inlined HTML dashboard. The page is fully
//!   static — every time-varying field is filled in client-side via
//!   cross-origin fetches against the REST listener. Hash-routed SPA
//!   with a top nav bar, a dual-axis hashrate/temperature chart, a
//!   right-side telemetry panel, and a configuration form view.
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

use edge_http::io::server::Connection;
use edge_http::io::Error as HttpError;
use edge_nal::TcpSplit;
use embassy_net::Stack;
use embedded_io_async::{Read, Write};
use heapless::Vec;
use portable_atomic::Ordering;
use static_cell::ConstStaticCell;
use ubos_core::consts::Http;
use ubos_net::net::REQUEST_COUNT;

use crate::{INDEX_HTML_GZ, LOGIN_HTML_GZ, LOGO_SVG};
use ubos_http::{
    extract_request_parts, format_content_length, read_body, write_status_only, RequestParts,
    Service, CONTENT_LENGTH_DECIMAL_DIGITS, MAX_RESPONSE_HEADERS,
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
            &[("Location", location), ("Connection", "close")],
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
                ("Connection", "close"),
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
                ("Connection", "close"),
            ],
        )
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
    let mut length_buffer = [0u8; CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = format_content_length(&mut length_buffer, body.len());

    // Assemble headers on the stack. The capacity is
    // MAX_RESPONSE_HEADERS; each push is infallible within that budget
    // because the slots used here are counted statically: 4 fixed plus
    // 1 optional encoding plus extra_headers up to 2, totalling at most
    // 6, within the 8-slot cap.
    let mut headers: Vec<(&str, &str), MAX_RESPONSE_HEADERS> = Vec::new();
    headers
        .push(("Content-Type", content_type))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Cache-Control", cache_control))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Connection", "close"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", content_length))
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

#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static HTTP_TCP_BUFFERS: ConstStaticCell<
    edge_nal_embassy::TcpBuffers<1, { Http::TX_BUFFER_SIZE }, { Http::RX_BUFFER_SIZE }>,
> = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static HTTP_WORK_BUFFER: ConstStaticCell<[u8; Http::REQUEST_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Http::REQUEST_BUFFER_SIZE]);

/// Dashboard handler implementing `ubos_http::Service`.
///
/// Serves the web-UI shell on port 80 (HTML, login, logout, logo).
/// Methods are enforced: wrong-method requests on known paths return
/// 405. `REQUEST_COUNT` increments on every accepted connection
/// regardless of response status, so the metric reflects accept-level
/// traffic rather than success rate.
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
        // Count every accepted connection so `board_http_requests_total`
        // reflects total traffic on the HTML/API port.
        REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);

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

        let needs_auth = path == "/logout";
        let auth_ctx = ubos_auth::auth::authenticate(authorization, cookie)
            .await
            .unwrap_or(ubos_auth::auth::AuthCtx::Anonymous);

        // Anonymous requests to authenticated paths are rejected.
        // RFC 7235 requires `WWW-Authenticate` on 401 so a Basic-auth
        // machine client knows the route accepts credentials and which
        // realm to retry against.
        if needs_auth && matches!(auth_ctx, ubos_auth::auth::AuthCtx::Anonymous) {
            return write_status_only(
                connection,
                401,
                "Unauthorized",
                "application/json",
                &[("WWW-Authenticate", "Basic realm=\"ubos\"")],
                b"{\"message\":\"unauthorized\"}",
            )
            .await;
        }

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

        // Session-authenticated state-changing requests require a valid
        // CSRF token in the X-CSRF-Token header that was issued for the
        // same session. Basic-authenticated requests are exempt.
        //
        // POST /login requires CSRF because a victim browser on the LAN
        // could otherwise be force-logged-in by an attacker page: the
        // attacker submits their own credentials into the victim's browser,
        // takes over the active session, and then observes any state the
        // victim stores in that session. The check applies only when a
        // session cookie is already present so that a fresh anonymous login
        // (no prior session) is not gated; the CSRF token is issued to the
        // existing session on `GET /` and reused here.
        let needs_csrf = needs_auth || (path == "/login" && is_mutating);
        if needs_csrf && is_mutating {
            if let ubos_auth::auth::AuthCtx::Session(ref session_id) = auth_ctx {
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

/// Accept HTTP connections and serve the web-UI shell. Runs forever.
///
/// Routes `GET /` to the inlined HTML dashboard, `GET /login`/`POST
/// /login`/`POST /logout` to the session flow, and `GET /logo.svg` to
/// the cacheable Braiins wordmark. Wrong-method requests on known
/// paths return 405. Anything else gets a 404. The transport loop
/// lives in `ubos-http`.
pub async fn run(stack: Stack<'static>) -> ! {
    // Populate the credential store before the accept loop so the very
    // first request finds the defaults in place and the operator sees
    // the default-credentials warning at boot rather than on first
    // login attempt.
    ubos_auth::auth::init_credentials().await;
    ubos_http::serve(
        stack,
        Http::PORT,
        DashboardService,
        HTTP_TCP_BUFFERS.take(),
        HTTP_WORK_BUFFER.take(),
    )
    .await
}
