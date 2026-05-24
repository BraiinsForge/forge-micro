//! REST endpoints under `/api/system/*`.
//!
//! Dispatched from `DashboardService::handle` ahead of the legacy
//! `/api/*` routes so the SPA's `/api/info` keeps working unchanged.
//! Each handler builds a value of the appropriate schema struct,
//! serializes it with `serde_json_core`, and wraps it in a JSON
//! response via `write_json_response`.
//!
//! Phase 5 adds stub action endpoints: POST /api/system/{pause,resume,
//! restart,identify,blockFound/dismiss} and PATCH /api/system. All
//! return 200 with `{"message":"ack"}` after emitting one `info!` log
//! line. No firmware state is changed. Method enforcement is strict:
//! every known path responds with 405 when the wrong method is used.

use core::fmt::Write as _;

use edge_http::io::server::Connection;
use edge_http::io::Error as HttpError;
use edge_nal::TcpSplit;
use embassy_net::Ipv4Address;
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use heapless::String;
use portable_atomic::Ordering;
use stratum_client::{SHARES_ACCEPTED, SHARES_REJECTED};
use ubos_core::fan_state::{FAN_PWM_PERCENT, FAN_RPM};
use ubos_logging::info;
use ubos_metrics::hashrate::{hashrate, SHARE_COUNT};
use ubos_miner::consts::NUM_CHIPS;
use ubos_miner::miner::VOLTAGE_MV;
use ubos_net::net::{LOCAL_IPV4, NTP_EPOCH_OFFSET, POOL_ALIVE_BITMAP, TEMPERATURE_C};

use crate::schema::{
    system_asic::{DEFAULT_FREQUENCY_MHZ, FREQUENCY_OPTIONS_MHZ, VOLTAGE_OPTIONS_MV},
    system_statistics::LABELS,
    GenericResponse, SystemAsic, SystemInfo, SystemScoreboardEntry, SystemStatistics, API_VARIANT,
};

/// Serialization scratch buffer size in bytes.
///
/// The largest schema struct is `SystemInfo`, which serializes to roughly
/// 600 bytes with all string fields at their maximums. 2048 bytes covers
/// that worst case and leaves headroom for fields added in later phases
/// without revisiting this constant.
///
/// Per-connection stack cost is `SCRATCH_SIZE` bytes; this multiplies by
/// `TCP_POOL` for concurrent connections. If you increase this constant,
/// audit task-stack sizing for all ubos-api consumers.
const SCRATCH_SIZE: usize = 2048;

/// Build the dotted-quad IPv4 string from the shared atomic.
///
/// Returns an empty string when DHCP has not yet assigned an address.
fn build_ip_string() -> String<16> {
    let mut result = String::new();
    let ip_bits = LOCAL_IPV4.load(Ordering::Relaxed);
    if ip_bits != 0 {
        let octets = Ipv4Address::from_bits(ip_bits).octets();
        let _ = core::write!(
            result,
            "{}.{}.{}.{}",
            octets[0],
            octets[1],
            octets[2],
            octets[3]
        );
    }
    result
}

/// Build the `HH:MM:SS UTC` time string from the NTP epoch offset.
///
/// Returns an empty string when NTP has not yet synchronized.
fn build_time_string() -> String<24> {
    let mut result = String::new();
    let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    if ntp_offset != 0 {
        let unix_secs = ubos_net::wall_clock_secs();
        let mut time_buffer = [0u8; 16];
        let length = ubos_net::ntp::format_utc_time(&mut time_buffer, unix_secs);
        if let Ok(time_str) = core::str::from_utf8(&time_buffer[..length]) {
            let _ = result.push_str(time_str);
        }
    }
    result
}

/// Write a JSON response to `connection` with the given status code and
/// pre-serialized body bytes.
///
/// `status` is the numeric HTTP status code. `reason` is the reason phrase.
/// `body` contains the serialized JSON bytes and determines the
/// `Content-Length` header value.
async fn write_json_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: u16,
    reason: &str,
    body: &[u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut length_buffer = [0u8; ubos_http::CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = ubos_http::format_content_length(&mut length_buffer, body.len());

    connection
        .initiate_response(
            status,
            Some(reason),
            &[
                ("Content-Type", "application/json"),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
                ("Content-Length", content_length),
            ],
        )
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Write a 405 Method Not Allowed response carrying the required
/// `Allow` header.
///
/// RFC 7231 §6.5.5 mandates the `Allow` header on every 405. The
/// routing table populates `allowed` per route so each response advertises
/// only the methods that route actually accepts.
async fn write_method_not_allowed<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    allowed: &'static str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = b"method not allowed";
    let mut length_buffer = [0u8; ubos_http::CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = ubos_http::format_content_length(&mut length_buffer, body.len());

    connection
        .initiate_response(
            405,
            Some("Method Not Allowed"),
            &[
                ("Allow", allowed),
                ("Content-Type", "text/plain"),
                ("Content-Length", content_length),
                ("Connection", "close"),
            ],
        )
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Serialize `value` to `scratch`, then write a 200 OK JSON response.
///
/// Falls back to a 500 plain-text response when `serde_json_core`
/// returns an error; the schema structs are plain serde derivations
/// so this path is treated as a firmware bug.
async fn serialize_and_respond<T, const N: usize, V: serde::Serialize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    value: &V,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if let Ok(length) = serde_json_core::to_slice(value, scratch) {
        write_json_response(connection, 200, "OK", &scratch[..length]).await
    } else {
        // BUG: serialization of a schema struct failed; the schemas are
        // plain serde derivations so this path should be unreachable.
        // Keep the body JSON to match the success-path Content-Type so
        // machine clients can parse the failure case with the same decoder.
        write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"message\":\"serialization error\"}",
        )
        .await
    }
}

/// Handle `GET /api/system/info`.
///
/// Populates all fields from the shared atomics. Fields that depend
/// on future phases are zeroed or empty per the plan.
async fn handle_info<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    board_name: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let uptime_seconds = Instant::now().as_secs();
    let hashrate_hps = hashrate();
    let temperature_celsius = TEMPERATURE_C.load(Ordering::Relaxed).cast_signed();
    let fan_speed_rpm = FAN_RPM.load(Ordering::Relaxed);
    let fan_pwm_percent = match FAN_PWM_PERCENT.load(Ordering::Relaxed) {
        u8::MAX => 0,
        percent => percent,
    };
    let pools_bitmap = POOL_ALIVE_BITMAP.load(Ordering::Relaxed);
    let shares_accepted = SHARES_ACCEPTED.load(Ordering::Relaxed);
    let shares_rejected = SHARES_REJECTED.load(Ordering::Relaxed);
    let shares_total = SHARE_COUNT.load(Ordering::Relaxed);

    let info = SystemInfo {
        api_variant: API_VARIANT,
        board_name,
        hostname: board_name,
        hashrate: hashrate_hps,
        uptime_seconds,
        temperature_celsius,
        fan_speed_rpm,
        fan_pwm_percent,
        pools_bitmap,
        ip: build_ip_string(),
        time: build_time_string(),
        asic_count: NUM_CHIPS,
        asic_model: "BM1362",
        default_frequency_mhz: DEFAULT_FREQUENCY_MHZ,
        default_voltage_mv: VOLTAGE_MV,
        shares_accepted,
        shares_rejected,
        shares_total,
        best_nonce_diff: 0,
        best_session_nonce_diff: 0,
    };

    serialize_and_respond(connection, scratch, &info).await
}

/// Handle `GET /api/system/asic`.
///
/// Returns static capability metadata for the BM1362 chip.
async fn handle_asic<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let asic = SystemAsic {
        asic_model: "BM1362",
        device_model: "miniminer",
        swarm_color: "blue",
        asic_count: NUM_CHIPS,
        default_frequency: DEFAULT_FREQUENCY_MHZ,
        frequency_options: FREQUENCY_OPTIONS_MHZ,
        default_voltage: VOLTAGE_MV,
        voltage_options: VOLTAGE_OPTIONS_MV,
    };

    serialize_and_respond(connection, scratch, &asic).await
}

/// Handle `GET /api/system/statistics`.
///
/// Returns the fixed label set with an empty rows slice. A ring buffer
/// will be wired in a later phase without a schema change.
async fn handle_statistics<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let ntp_offset = NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    let current_timestamp = if ntp_offset != 0 {
        ubos_net::wall_clock_secs().saturating_mul(1_000)
    } else {
        0
    };

    let statistics = SystemStatistics {
        current_timestamp,
        labels: LABELS,
        statistics: &[],
    };

    serialize_and_respond(connection, scratch, &statistics).await
}

/// Handle `GET /api/system/scoreboard`.
///
/// Serializes an empty `SystemScoreboardEntry` slice as a JSON array.
/// The scoreboard ring buffer is wired in a later phase.
async fn handle_scoreboard<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let entries: &[SystemScoreboardEntry] = &[];
    if let Ok(length) = serde_json_core::to_slice(entries, scratch) {
        write_json_response(connection, 200, "OK", &scratch[..length]).await
    } else {
        // BUG: serializing an empty slice failed; this is a serde_json_core
        // invariant violation.
        write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"error\":\"serialization\"}",
        )
        .await
    }
}

/// Handle a not-implemented endpoint with a 501 JSON response.
async fn handle_not_implemented<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    message: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = GenericResponse { message };
    if let Ok(length) = serde_json_core::to_slice(&body, scratch) {
        write_json_response(connection, 501, "Not Implemented", &scratch[..length]).await
    } else {
        // BUG: serializing GenericResponse failed; the struct is a plain
        // serde derivation so this path is unreachable in practice.
        write_json_response(
            connection,
            501,
            "Not Implemented",
            b"{\"error\":\"serialization\"}",
        )
        .await
    }
}

/// Write a 200 OK response with `{"message":"ack"}` body.
///
/// Shared by every stub action handler. The body signals that the
/// request was received; no firmware state is changed.
async fn write_ack_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = GenericResponse { message: "ack" };
    if let Ok(length) = serde_json_core::to_slice(&body, scratch) {
        write_json_response(connection, 200, "OK", &scratch[..length]).await
    } else {
        // BUG: serializing GenericResponse { message: "ack" } failed;
        // the struct is a plain serde derivation so this path is unreachable.
        write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"error\":\"serialization\"}",
        )
        .await
    }
}

/// Handle a POST action endpoint (pause, resume, restart, identify,
/// blockFound/dismiss).
///
/// Logs one `info!` line naming the action and returns 200 with
/// `{"message":"ack"}`. No firmware state is changed by any of these
/// stubs; actual wiring is deferred to a later phase.
async fn handle_action<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    action: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    info!("api action: {}", action);
    write_ack_response(connection, scratch).await
}

/// Handle `PATCH /api/system`.
///
/// Logs the body byte count and returns 200 with `{"message":"ack"}`.
/// No fields are applied; the audit doc places credential and config
/// persistence out of scope for this phase. The body itself is not
/// logged because legitimate clients may include credentials in the
/// JSON payload, and the stub has no operational use for the contents.
async fn handle_patch_system<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    body_len: usize,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    info!("api patch /api/system ({} bytes)", body_len);
    write_ack_response(connection, scratch).await
}

/// A known route on the `/api/system/*` surface.
///
/// Returned by `route()` when the method and path both match a handled
/// endpoint. `dispatch()` switches on this to call the right handler.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum Route {
    /// `GET /api/system/info`
    SystemInfo,
    /// `GET /api/system/asic`
    SystemAsic,
    /// `GET /api/system/statistics`
    SystemStatistics,
    /// `GET /api/system/scoreboard`
    SystemScoreboard,
    /// `GET /api/system/logs`
    SystemLogs,
    /// `GET /api/system/wifi/scan`
    SystemWifiScan,
    /// `POST /api/system/OTA`
    SystemOta,
    /// `POST /api/system/OTAWWW`
    SystemOtaWww,
    /// `POST /api/system/pause`
    PauseAck,
    /// `POST /api/system/resume`
    ResumeAck,
    /// `POST /api/system/restart`
    RestartAck,
    /// `POST /api/system/identify`
    IdentifyAck,
    /// `POST /api/system/blockFound/dismiss`
    BlockFoundDismissAck,
    /// `PATCH /api/system`
    PatchSystem,
}

/// Outcome of `route()` for a single method+path pair.
///
/// Separates the pure routing decision from the response-writing so
/// the routing table can be unit-tested without a live `Connection`.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum DispatchOutcome {
    /// Known method on a known path — proceed to build the response.
    Handled(Route),
    /// Known path, wrong method — respond with 405 and this `Allow` value.
    MethodNotAllowed { allowed: &'static str },
    /// Unknown path — caller falls through to global 404.
    NotFound,
}

/// Classify a method+path pair against the `/api/system/*` surface.
///
/// The function is pure (no I/O, no state reads) so it can be called
/// from host unit tests. `dispatch()` calls this first and then drives
/// the `Connection` based on the result.
pub(crate) fn route(method: &str, path: &str) -> DispatchOutcome {
    match path {
        "/api/system/info" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemInfo),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/asic" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemAsic),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/statistics" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemStatistics),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/scoreboard" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemScoreboard),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/logs" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemLogs),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/wifi/scan" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemWifiScan),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/OTA" => match method {
            "POST" => DispatchOutcome::Handled(Route::SystemOta),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/OTAWWW" => match method {
            "POST" => DispatchOutcome::Handled(Route::SystemOtaWww),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/pause" => match method {
            "POST" => DispatchOutcome::Handled(Route::PauseAck),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/resume" => match method {
            "POST" => DispatchOutcome::Handled(Route::ResumeAck),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/restart" => match method {
            "POST" => DispatchOutcome::Handled(Route::RestartAck),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/identify" => match method {
            "POST" => DispatchOutcome::Handled(Route::IdentifyAck),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/blockFound/dismiss" => match method {
            "POST" => DispatchOutcome::Handled(Route::BlockFoundDismissAck),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system" => match method {
            "PATCH" => DispatchOutcome::Handled(Route::PatchSystem),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "PATCH" },
        },
        _ => DispatchOutcome::NotFound,
    }
}

/// Try to dispatch a `/api/system/*` request.
///
/// Returns `true` when the path belongs to the system surface (whether
/// the method matched or produced 405). Returns `false` only when the
/// path does not belong to this surface at all, so the caller falls
/// through to legacy routing.
///
/// `body` carries the already-read request body bytes. The length of
/// the slice is all the body content available; callers must read it
/// before calling this function.
///
/// Crate-private because the auth/CSRF/allow-list gates live in
/// `DashboardService::handle`; exposing this directly would let an
/// outside caller bypass those checks.
pub(crate) async fn dispatch<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    method: &str,
    path: &str,
    body: &[u8],
    board_name: &str,
) -> Result<bool, HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    // One serialization scratch per dispatch invocation. The handlers
    // are mutually exclusive so a single buffer is sufficient; sharing
    // it keeps peak stack pressure to a single 2 KiB allocation rather
    // than one per handler.
    let mut scratch = [0u8; SCRATCH_SIZE];
    let scratch = &mut scratch[..];

    match route(method, path) {
        DispatchOutcome::NotFound => return Ok(false),
        DispatchOutcome::MethodNotAllowed { allowed } => {
            write_method_not_allowed(connection, allowed).await?;
        }
        DispatchOutcome::Handled(Route::SystemInfo) => {
            handle_info(connection, scratch, board_name).await?;
        }
        DispatchOutcome::Handled(Route::SystemAsic) => {
            handle_asic(connection, scratch).await?;
        }
        DispatchOutcome::Handled(Route::SystemStatistics) => {
            handle_statistics(connection, scratch).await?;
        }
        DispatchOutcome::Handled(Route::SystemScoreboard) => {
            handle_scoreboard(connection, scratch).await?;
        }
        DispatchOutcome::Handled(Route::SystemLogs) => {
            handle_not_implemented(connection, scratch, "not implemented").await?;
        }
        DispatchOutcome::Handled(
            Route::SystemWifiScan | Route::SystemOta | Route::SystemOtaWww,
        ) => {
            handle_not_implemented(connection, scratch, "unsupported on this platform").await?;
        }
        DispatchOutcome::Handled(Route::PauseAck) => {
            handle_action(connection, scratch, "pause").await?;
        }
        DispatchOutcome::Handled(Route::ResumeAck) => {
            handle_action(connection, scratch, "resume").await?;
        }
        DispatchOutcome::Handled(Route::RestartAck) => {
            handle_action(connection, scratch, "restart").await?;
        }
        DispatchOutcome::Handled(Route::IdentifyAck) => {
            handle_action(connection, scratch, "identify").await?;
        }
        DispatchOutcome::Handled(Route::BlockFoundDismissAck) => {
            handle_action(connection, scratch, "blockFound/dismiss").await?;
        }
        DispatchOutcome::Handled(Route::PatchSystem) => {
            handle_patch_system(connection, scratch, body.len()).await?;
        }
    }

    Ok(true)
}

#[cfg(test)]
#[path = "tests/system.rs"]
mod tests;
