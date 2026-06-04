//! Dashboard service and HTTP handler for the firmware web UI.
//!
//! Four legacy endpoints are handled by `DashboardService` alongside
//! the `/api/system/*` surface dispatched by `system::dispatch`:
//!
//! - `GET /` returns the inlined HTML dashboard. The page is fully
//!   static — every time-varying field is filled in client-side from
//!   `/api/info`. Hash-routed single-page app with a top nav bar, a
//!   dual-axis hashrate/temperature chart, a right-side telemetry
//!   panel, and a configuration form view. Inlined CSS and JS keep
//!   the first paint to a single response that fits in one TCP segment,
//!   enabling one-RTT delivery on LAN.
//! - `GET /logo.svg` serves the Braiins wordmark referenced by the
//!   dashboard's nav bar. Kept as a separate cacheable asset rather
//!   than inlined so the index response stays inside its buffer.
//! - `GET /api/info` returns a compact JSON telemetry blob. The
//!   dashboard polls it every 2 s and pushes hashrate and temperature
//!   samples into ring buffers rendered as SVG polylines.
//! - `GET /api/config` returns the current pool list as a
//!   [`ubos_config::PoolUpdate`]. The dashboard fetches it on page
//!   load to populate the form with the values the firmware is
//!   already running. The response carries only the `pool` field by
//!   construction, so `ApiConfig` (the dashboard's own basic-auth
//!   credentials), `ThermalConfig`, and `CoolingConfig` stay inside
//!   the firmware. Pool worker credentials are part of the form
//!   payload by design: the operator entered them and the dashboard
//!   must round-trip them to support editing.
//! - `POST /api/config` accepts a [`ubos_config::PoolUpdate`] JSON
//!   body, merges it into the current [`ubos_config::Config`] from
//!   [`ubos_config::handle`], and forwards the result as
//!   [`ubos_config::Message::Update`]. The dashboard parses its own
//!   URL syntax in JS before sending; the firmware stays a typed
//!   sink.
//!
//! Any other path returns a 404 with a plain-text body. Wrong-method
//! requests on known paths return 405. The transport loop and request parsing live in
//! `ubos-http`; this module owns the handler dispatch and the response
//! streaming through `edge-http`'s `Connection` state machine.

use core::fmt::Write as _;

use edge_http::io::server::Connection;
use edge_http::io::Error as HttpError;
use edge_nal::TcpSplit;
use embassy_net::{Ipv4Address, Stack};
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use heapless::Vec;
use portable_atomic::Ordering;
use static_cell::ConstStaticCell;
use ubos_core::consts::Http;
use ubos_core::fan_state;
use ubos_logging::{info, warn};
use ubos_manager::Handle as ManagerHandle;
use ubos_net::net::{LOCAL_IPV4, NTP_EPOCH_OFFSET, POOL_ALIVE_BITMAP, REQUEST_COUNT};
use ubos_thermal::Handle as ThermalHandle;
use ubos_webui::{INDEX_HTML_GZ, LOGIN_HTML_GZ, LOGO_SVG};

use ubos_http::{format_content_length, write_status_only, Service, CONTENT_LENGTH_DECIMAL_DIGITS};

#[cfg(test)]
#[path = "tests/dashboard.rs"]
mod tests;

/// Maximum number of response headers built per request.
///
/// Most responses use 4 headers (Content-Type, Cache-Control,
/// Connection, Content-Length). Session responses need an additional
/// Set-Cookie or X-CSRF-Token header. Eight slots cover the worst-case
/// header count (4 fixed + 1 encoding + 1 extra = 6) with two slots to
/// spare.
pub(crate) const MAX_RESPONSE_HEADERS: usize = 8;

/// Body buffer size for JSON responses built by `/api/info`.
///
/// The largest `/api/info` JSON body with all fields populated is
/// roughly 200 bytes (name≤32 + numeric fields + 16-byte IPv4 + 16-byte
/// time string + JSON framing). 512 bytes leaves room for future field
/// additions while staying on the stack.
pub(crate) const API_INFO_BODY_BUFFER_SIZE: usize = 512;

/// Body buffer for the POST /api/config acknowledgement.
pub(crate) const CONFIG_ACK_BODY: &[u8] = b"{\"ok\":true}";

/// Body buffer size for `GET /api/config` responses.
///
/// Each [`ubos_config::PoolEntry`] serialises to at most ~460 bytes
/// (three 128-byte heapless strings plus JSON framing); three entries
/// plus the outer `{"pool":[…]}` wrapper fit inside 1.5 KiB with margin.
pub(crate) const CONFIG_RESPONSE_BUFFER_SIZE: usize = 1536;

/// Buffer for reading POST/PATCH request bodies.
///
/// Sized to `Http::REQUEST_BUFFER_SIZE` so a login form or config
/// payload fits in one allocation. Requests that exceed this cap
/// receive a 413 response.
pub(crate) const BODY_READ_BUFFER_SIZE: usize = Http::REQUEST_BUFFER_SIZE;

/// Validate that a board name string contains no JSON-unsafe characters.
///
/// Returns `true` when `name` contains no `"` (0x22) or `\` (0x5c)
/// bytes. Any such byte would break the unescaped JSON string literal
/// in the `/api/info` response body, so callers that inject board names
/// into JSON must enforce this invariant at compile time.
pub(crate) const fn validate_board_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x22 || bytes[index] == 0x5c {
            return false;
        }
        index += 1;
    }
    true
}

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
pub(crate) async fn write_asset<T, const N: usize>(
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

    // Assemble headers into a small stack array. The maximum is
    // MAX_RESPONSE_HEADERS; each push is infallible within that budget
    // because the slots used here are counted statically (4 fixed +
    // 1 optional encoding + extra_headers ≤ 2, totalling at most 6,
    // well within the 8-slot cap).
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

/// Write a JSON response with `body` bytes.
pub(crate) async fn write_json_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    body: &[u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    write_json_response_with_extra(connection, body, &[]).await
}

/// Write a JSON response with `body` bytes and optional extra headers.
///
/// Used by the REST listener to attach CORS headers to JSON responses.
/// `extra_headers` is appended after the four fixed headers; the total
/// must not exceed `MAX_RESPONSE_HEADERS`.
pub(crate) async fn write_json_response_with_extra<T, const N: usize>(
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

/// Read up to `BODY_READ_BUFFER_SIZE` bytes of request body via the
/// `Connection::Read` impl.
///
/// Called from both the dashboard and REST listeners.
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
pub(crate) async fn read_body<'buf, T, const N: usize>(
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

/// Decoded form-field length cap. Larger inputs are rejected.
const FORM_FIELD_MAX_LEN: usize = 64;

/// Extract a URL-encoded form field value from a `key=value&...` body,
/// percent-decoded.
///
/// Returns the decoded value, or `None` when the field is missing, the
/// decoded value exceeds [`FORM_FIELD_MAX_LEN`], or the value contains
/// malformed `%`-escapes or non-UTF-8 bytes. `+` is decoded as a space
/// per `application/x-www-form-urlencoded`.
pub(crate) fn form_field(body: &str, key: &str) -> Option<heapless::String<FORM_FIELD_MAX_LEN>> {
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
pub(crate) fn percent_decode(input: &str) -> Option<heapless::String<FORM_FIELD_MAX_LEN>> {
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

static HTTP_TCP_BUFFERS: ConstStaticCell<
    edge_nal_embassy::TcpBuffers<1, { Http::TX_BUFFER_SIZE }, { Http::RX_BUFFER_SIZE }>,
> = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static HTTP_WORK_BUFFER: ConstStaticCell<[u8; Http::REQUEST_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Http::REQUEST_BUFFER_SIZE]);

/// Dashboard handler implementing `ubos_http::Service`.
///
/// Holds the board name used in `/api/info` responses and dispatches
/// each request to the appropriate response builder. Methods are
/// enforced: wrong-method requests on known paths return 405.
/// `REQUEST_COUNT` increments on every accepted connection regardless of
/// response status, so the metric reflects accept-level traffic rather
/// than success rate.
pub struct DashboardService {
    board_name: &'static str,
    thermal: ThermalHandle,
    manager: ManagerHandle,
}

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

        // Extract all header values we need into owned stack locals
        // before any mutation of the connection. The headers borrow
        // `connection` immutably; copying values out into owned strings
        // releases that borrow so `read_body` can take a mutable borrow
        // for the body read later.
        let (method_buf, path_buf, authorization_buf, cookie_buf, csrf_buf, content_length) = {
            let headers = connection
                .headers()
                .map_err(|_| edge_http::io::Error::InvalidState)?;

            // `Method::as_str` is private; use the `Display` impl.
            let mut method = heapless::String::<16>::new();
            let _ = core::write!(method, "{}", headers.method);

            let mut path = heapless::String::<128>::new();
            // Paths longer than the buffer are left empty so the route
            // match below falls through to 404 instead of being silently
            // remapped to a valid route.
            if headers.path.len() <= 128 {
                let _ = path.push_str(headers.path);
            }

            let authorization: Option<heapless::String<128>> =
                headers.headers.get("Authorization").and_then(|value| {
                    let mut owned = heapless::String::<128>::new();
                    if owned.push_str(value).is_err() {
                        warn!("Authorization header truncated, treating as missing");
                        return None;
                    }
                    Some(owned)
                });

            let cookie: Option<heapless::String<256>> =
                headers.headers.get("Cookie").and_then(|value| {
                    let mut owned = heapless::String::<256>::new();
                    if owned.push_str(value).is_err() {
                        warn!("Cookie header truncated, treating as missing");
                        return None;
                    }
                    Some(owned)
                });

            let csrf: Option<heapless::String<64>> =
                headers.headers.get("X-CSRF-Token").and_then(|value| {
                    let mut owned = heapless::String::<64>::new();
                    owned.push_str(value).ok()?;
                    Some(owned)
                });

            let content_length = headers.headers.content_len();

            (method, path, authorization, cookie, csrf, content_length)
        };

        let method = method_buf.as_str();
        let path = path_buf.as_str();
        let authorization = authorization_buf.as_deref();
        let cookie = cookie_buf.as_deref();

        let needs_auth = path.starts_with("/api/") || path == "/logout";
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

        if path == "/api/system" || path.starts_with("/api/system/") {
            let handled = crate::system::dispatch(
                connection,
                method,
                path,
                body,
                self.board_name,
                self.thermal,
                self.manager,
                &[],
            )
            .await?;
            if !handled {
                return write_status_only(
                    connection,
                    404,
                    "Not Found",
                    "text/plain",
                    &[],
                    b"not found",
                )
                .await;
            }
            return Ok(());
        }

        match (method, path) {
            ("GET", "/") => {
                match auth_ctx {
                    ubos_auth::auth::AuthCtx::Anonymous => write_redirect(connection, "/login").await,
                    ubos_auth::auth::AuthCtx::Session(ref session_id) => {
                        let csrf_token = ubos_auth::csrf::issue_csrf_token(session_id.as_str()).await;
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
            ("GET", "/api/info") => {
                let uptime_secs = Instant::now().as_secs();
                let hashrate = ubos_metrics::hashrate::hashrate();
                #[allow(clippy::cast_possible_truncation)]
                let temperature = self
                    .thermal
                    .current()
                    .map_or(0i32, |r| r.chip_max.celsius_f32() as i32);
                let fan_rpm = fan_state::load_rpm().rpm();
                let fan_pwm = fan_state::load_pwm().percent();
                let pools = POOL_ALIVE_BITMAP.load(Ordering::Relaxed);

                // Build the JSON body into a stack buffer.
                let mut body_buf = [0u8; API_INFO_BODY_BUFFER_SIZE];
                // Use a BufferWriter to fill body_buf with the JSON.
                let mut writer = ubos_http::BufferWriter::new(&mut body_buf);

                // `board_name` is interpolated without JSON escaping. The
                // caller must guarantee the value passes `validate_board_name`.
                // Each consumer crate enforces this at compile time with:
                //   `const _: () = assert!(validate_board_name(Board::NAME), ...);`
                // The debug_assert here catches violations in tests and debug
                // builds; release builds rely on the consumer-side const assert.
                debug_assert!(
                    validate_board_name(self.board_name),
                    "BUG: board_name contains JSON-unsafe characters"
                );
                let _ = core::write!(
                    writer,
                    "{{\"name\":\"{}\",\
                     \"hashrate\":{hashrate},\
                     \"uptime\":{uptime_secs},\
                     \"temperature\":{temperature},\
                     \"fan_rpm\":{fan_rpm},\
                     \"fan_pwm\":{fan_pwm},\
                     \"pools\":{pools},\
                     \"ip\":\"",
                    self.board_name
                );

                let ip_bits = LOCAL_IPV4.load(Ordering::Relaxed);
                if ip_bits != 0 {
                    let octets = Ipv4Address::from_bits(ip_bits).octets();
                    let _ = core::write!(
                        writer,
                        "{}.{}.{}.{}",
                        octets[0],
                        octets[1],
                        octets[2],
                        octets[3]
                    );
                }

                let _ = writer.write_str("\",\"time\":\"");
                let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
                if ntp_offset != 0 {
                    let unix_secs = ubos_net::wall_clock_secs();
                    let mut time_buffer = [0u8; 16];
                    let length = ubos_net::ntp::format_utc_time(&mut time_buffer, unix_secs);
                    if let Ok(time_str) = core::str::from_utf8(&time_buffer[..length]) {
                        let _ = writer.write_str(time_str);
                    }
                }
                let _ = writer.write_str("\"}");
                let json_len = writer.len();

                write_json_response(connection, &body_buf[..json_len]).await
            }
            ("GET", "/api/config") => {
                let snapshot = ubos_config::handle().current();
                let response = ubos_config::PoolUpdate {
                    pool: snapshot.pool,
                };
                let mut body_buf = [0u8; CONFIG_RESPONSE_BUFFER_SIZE];
                if let Ok(length) = serde_json_core::ser::to_slice(&response, &mut body_buf) {
                    write_json_response(connection, &body_buf[..length]).await
                } else {
                    warn!("config: failed to serialize PoolUpdate response");
                    write_status_only(
                        connection,
                        500,
                        "Internal Server Error",
                        "application/json",
                        &[],
                        b"{\"message\":\"config serialize failed\"}",
                    )
                    .await
                }
            }
            ("POST", "/api/config") => {
                info!("pool config update ({} bytes)", body.len());
                if let Ok((update, _)) =
                    serde_json_core::de::from_slice::<ubos_config::PoolUpdate>(body)
                {
                    let config_handle = ubos_config::handle();
                    let mut new_config = config_handle.current();
                    new_config.pool = update.pool;
                    config_handle
                        .send(ubos_config::Message::Update(new_config))
                        .await;
                    write_json_response(connection, CONFIG_ACK_BODY).await
                } else {
                    warn!("config: rejected POST /api/config with invalid JSON");
                    write_status_only(
                        connection,
                        400,
                        "Bad Request",
                        "application/json",
                        &[],
                        b"{\"message\":\"invalid pool update JSON\"}",
                    )
                    .await
                }
            }
            (_, "/" | "/login" | "/logout" | "/logo.svg" | "/api/info" | "/api/config") => {
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

/// Accept HTTP connections and serve the dashboard. Runs forever.
///
/// Routes `GET /` to the inlined HTML dashboard, `GET /logo.svg` to
/// the cacheable Braiins wordmark, `GET /api/info` to a small JSON
/// telemetry blob the dashboard polls, and `POST /api/config` to a
/// stub handler that logs the submitted body before returning a 200
/// OK. Wrong-method requests on known paths return 405. Anything else
/// gets a 404. The transport loop lives in `ubos-http`.
pub async fn run(
    stack: Stack<'static>,
    board_name: &'static str,
    thermal: ThermalHandle,
    manager: ManagerHandle,
) -> ! {
    // Populate the credential store before the accept loop so the very
    // first request finds the defaults in place and the operator sees
    // the default-credentials warning at boot rather than on first
    // login attempt.
    ubos_auth::auth::init_credentials().await;
    ubos_http::serve(
        stack,
        Http::PORT,
        DashboardService {
            board_name,
            thermal,
            manager,
        },
        HTTP_TCP_BUFFERS.take(),
        HTTP_WORK_BUFFER.take(),
    )
    .await
}
