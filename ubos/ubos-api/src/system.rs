//! REST endpoints under `/api/system/*`.
//!
//! Dispatched from `RestApiService::handle` on the REST listener
//! (port 8080). Each handler builds a value of the appropriate
//! schema struct, serializes it with `serde_json_core`, and wraps it
//! in a JSON response via `write_json_response`.
//!
//! Action endpoints (`POST /api/system/{pause,resume,restart,identify,
//! blockFound/dismiss,factory_reset}` and `PATCH /api/system`) are
//! handled as follows:
//!
//! - `restart` — writes a 200 ack, then calls
//!   [`ubos_core::reboot::request_reboot`] so the board's reset task
//!   performs the actual MCU reset after the socket drains.
//! - `factory_reset` — sends [`ubos_config::Message::FactoryReset`] to
//!   the config task, which wipes persisted config back to defaults and
//!   then requests the reboot once the wipe is persisted; the handler
//!   only writes the 200 ack.
//! - `identify`, `blockFound/dismiss`, and `PATCH /api/system` — stubs
//!   that return 200 with `{"message":"ack"}` and change no firmware
//!   state; full implementations are planned for later phases.
//!
//! Method enforcement is strict: every known path responds with 405
//! when the wrong method is used.

use core::fmt::Write as _;

use edge_http::io::server::Connection;
use edge_http::io::Error as HttpError;
use edge_nal::TcpSplit;
use embassy_net::{Ipv4Address, Stack};
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use heapless::String;
use portable_atomic::Ordering;
use stratum_client::{SHARES_ACCEPTED, SHARES_REJECTED};
use ubos_core::fan_state;
use ubos_logging::info;
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_metrics::hashrate::{hashrate, SHARE_COUNT};
use ubos_net::net::{LOCAL_IPV4, NTP_EPOCH_OFFSET, POOL_ALIVE_BITMAP};
use ubos_thermal::Handle as ThermalHandle;

use crate::schema::{
    system_statistics::LABELS, GenericResponse, SystemInfo, SystemNetwork, SystemScoreboardEntry,
    SystemStatistics, API_VARIANT,
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

/// Maximum number of response headers built per system dispatch response.
///
/// Four fixed headers (Content-Type, Cache-Control, Connection,
/// Content-Length) plus up to two CORS headers from the REST listener
/// equals six. Eight slots leave headroom for future additions.
const MAX_SYSTEM_RESPONSE_HEADERS: usize = 8;

/// Write a JSON response to `connection` with the given status code and
/// pre-serialized body bytes.
///
/// `status` is the numeric HTTP status code. `reason` is the reason phrase.
/// `body` contains the serialized JSON bytes and determines the
/// `Content-Length` header value. `extra_headers` carries caller-supplied
/// headers such as CORS headers from the REST listener.
async fn write_json_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: u16,
    reason: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut length_buffer = [0u8; ubos_http::CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = ubos_http::format_content_length(&mut length_buffer, body.len());

    let mut headers: heapless::Vec<(&str, &str), MAX_SYSTEM_RESPONSE_HEADERS> =
        heapless::Vec::new();
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
        .initiate_response(status, Some(reason), &headers)
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Write a 405 Method Not Allowed response carrying the required
/// `Allow` header.
///
/// RFC 7231 §6.5.5 mandates the `Allow` header on every 405. The
/// routing table populates `allowed` per route so each response advertises
/// only the methods that route actually accepts. `extra_headers` carries
/// caller-supplied headers such as CORS headers from the REST listener.
async fn write_method_not_allowed<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    allowed: &'static str,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = b"method not allowed";
    let mut length_buffer = [0u8; ubos_http::CONTENT_LENGTH_DECIMAL_DIGITS];
    let content_length = ubos_http::format_content_length(&mut length_buffer, body.len());

    let mut headers: heapless::Vec<(&str, &str), MAX_SYSTEM_RESPONSE_HEADERS> =
        heapless::Vec::new();
    headers
        .push(("Allow", allowed))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Type", "text/plain"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", content_length))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Connection", "close"))
        .expect("BUG: response header vec sized for max headers");
    for &header in extra_headers {
        headers
            .push(header)
            .expect("BUG: response header vec sized for max headers");
    }

    connection
        .initiate_response(405, Some("Method Not Allowed"), &headers)
        .await?;

    embedded_io_async::Write::write_all(connection, body).await?;

    Ok(())
}

/// Serialize `value` to `scratch`, then write a 200 OK JSON response.
///
/// Falls back to a 500 plain-text response when `serde_json_core`
/// returns an error; the schema structs are plain serde derivations
/// so this path is treated as a firmware bug. `extra_headers` carries
/// caller-supplied headers such as CORS headers from the REST listener.
async fn serialize_and_respond<T, const N: usize, V: serde::Serialize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    value: &V,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if let Ok(length) = serde_json_core::to_slice(value, scratch) {
        write_json_response(connection, 200, "OK", &scratch[..length], extra_headers).await
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
            extra_headers,
        )
        .await
    }
}

/// Handle `GET /api/system/info`.
///
/// Populates all fields from the shared atomics and the thermal handle.
/// Fields that depend on future phases are zeroed or empty per the plan.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_info<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    board_name: &str,
    thermal: ThermalHandle,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let uptime_seconds = Instant::now().as_secs();
    let hashrate_hps = hashrate();
    #[allow(clippy::cast_possible_truncation)]
    let temperature_celsius = thermal
        .current()
        .map_or(0i32, |r| r.chip_max.celsius_f32() as i32);
    let fan_speed_rpm = fan_state::load_rpm().rpm();
    let fan_pwm_percent = fan_state::load_pwm().percent();
    let pools_bitmap = POOL_ALIVE_BITMAP.load(Ordering::Relaxed);
    let shares_accepted = SHARES_ACCEPTED.load(Ordering::Relaxed);
    let shares_rejected = SHARES_REJECTED.load(Ordering::Relaxed);
    let shares_total = SHARE_COUNT.load(Ordering::Relaxed);
    // Extract the configured hostname before building SystemInfo so the
    // full Config snapshot is dropped before the serialize_and_respond
    // await, keeping the response future small.
    let configured_hostname = ubos_config::handle().current().network.hostname;
    let hostname = if configured_hostname.is_empty() {
        board_name
    } else {
        configured_hostname.as_str()
    };

    let info = SystemInfo {
        api_variant: API_VARIANT,
        board_name,
        hostname,
        hashrate: hashrate_hps,
        uptime_seconds,
        temperature_celsius,
        fan_speed_rpm,
        fan_pwm_percent,
        pools_bitmap,
        ip: build_ip_string(),
        time: build_time_string(),
        asic_count: ubos_config::MINING_INFO.asic_count,
        asic_model: ubos_config::MINING_INFO.asic_model,
        default_frequency_mhz: ubos_config::MINING_INFO.default_frequency_mhz,
        default_voltage_mv: ubos_config::MINING_INFO.default_voltage_mv,
        shares_accepted,
        shares_rejected,
        shares_total,
        best_nonce_diff: 0,
        best_session_nonce_diff: 0,
    };

    serialize_and_respond(connection, scratch, &info, extra_headers).await
}

/// Handle `GET /api/system/asic`.
///
/// Returns static capability metadata for the BM1362 chip.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_asic<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(
        connection,
        scratch,
        &ubos_config::MINING_INFO,
        extra_headers,
    )
    .await
}

/// Handle `GET /api/system/thermal`.
///
/// Returns the accepted ranges for the Temperature config section so the
/// webui pulls its input `min`/`max` from the same source the firmware
/// validates against, matching the `/api/system/asic` precedent.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_thermal<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(
        connection,
        scratch,
        &ubos_config::THERMAL_INFO,
        extra_headers,
    )
    .await
}

/// Handle `GET /api/system/cooling`.
///
/// Returns the accepted ranges for the Cooling config section so the
/// webui pulls its `targetC` input `min`/`max` from the same source
/// the firmware validates against. Mirrors `handle_thermal` for the
/// cooling domain, following the `MiningInfo` precedent of keeping
/// bounds metadata at a dedicated endpoint.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_cooling<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(
        connection,
        scratch,
        &ubos_config::COOLING_INFO,
        extra_headers,
    )
    .await
}

/// Handle `GET /api/system/statistics`.
///
/// Returns the fixed label set with an empty rows slice. A ring buffer
/// will be wired in a later phase without a schema change.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_statistics<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
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

    serialize_and_respond(connection, scratch, &statistics, extra_headers).await
}

/// Handle `GET /api/system/scoreboard`.
///
/// Serializes an empty `SystemScoreboardEntry` slice as a JSON array.
/// The scoreboard ring buffer is wired in a later phase.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_scoreboard<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let entries: &[SystemScoreboardEntry] = &[];
    if let Ok(length) = serde_json_core::to_slice(entries, scratch) {
        write_json_response(connection, 200, "OK", &scratch[..length], extra_headers).await
    } else {
        // BUG: serializing an empty slice failed; this is a serde_json_core
        // invariant violation.
        write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"message\":\"serialization error\"}",
            extra_headers,
        )
        .await
    }
}

/// Handle a not-implemented endpoint with a 501 JSON response.
///
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_not_implemented<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    message: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = GenericResponse { message };
    if let Ok(length) = serde_json_core::to_slice(&body, scratch) {
        write_json_response(
            connection,
            501,
            "Not Implemented",
            &scratch[..length],
            extra_headers,
        )
        .await
    } else {
        // BUG: serializing GenericResponse failed; the struct is a plain
        // serde derivation so this path is unreachable in practice.
        write_json_response(
            connection,
            501,
            "Not Implemented",
            b"{\"message\":\"serialization error\"}",
            extra_headers,
        )
        .await
    }
}

/// Write a 200 OK response with `{"message":"ack"}` body.
///
/// Shared by every stub action handler. The body signals that the
/// request was received; no firmware state is changed. `extra_headers`
/// carries caller-supplied headers such as CORS headers from the REST
/// listener.
async fn write_ack_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = GenericResponse { message: "ack" };
    if let Ok(length) = serde_json_core::to_slice(&body, scratch) {
        write_json_response(connection, 200, "OK", &scratch[..length], extra_headers).await
    } else {
        // BUG: serializing GenericResponse { message: "ack" } failed;
        // the struct is a plain serde derivation so this path is unreachable.
        write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"message\":\"serialization error\"}",
            extra_headers,
        )
        .await
    }
}

/// Handle `GET /api/system/network`.
///
/// Serializes the current network mode, pending-change flag, confirm
/// countdown, and live address fields as a [`SystemNetwork`] JSON body.
/// `stack` is the live embassy-net stack; `config_v4()` reads the current
/// address without any global state or unsafe code.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_network_get<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    stack: Stack<'static>,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let seconds_remaining = crate::network_apply::pending_seconds_remaining();
    // While a change is pending the persisted mode still reflects the old
    // config, but the live address already reflects the preview. Report the
    // previewed mode so the two agree until the operator confirms or it
    // reverts.
    let mode = match crate::network_apply::pending_mode_is_static() {
        Some(true) => "static",
        Some(false) => "dhcp",
        None => match ubos_config::handle().current().network.mode {
            ubos_config::NetworkMode::Dhcp => "dhcp",
            ubos_config::NetworkMode::Static { .. } => "static",
        },
    };
    let live = stack.config_v4();
    let address = live.as_ref().map(|c| c.address.address().octets());
    let prefix_len = live.as_ref().map(|c| c.address.prefix_len());
    let gateway = live.as_ref().and_then(|c| c.gateway.map(|gw| gw.octets()));
    let mut dns = heapless::Vec::new();
    if let Some(ref config) = live {
        for server in &config.dns_servers {
            // dns has the same capacity as dns_servers, so this never
            // overflows; the discarded result reflects that invariant.
            let _ = dns.push(server.octets());
        }
    }
    let status = SystemNetwork {
        mode,
        pending: seconds_remaining > 0,
        seconds_remaining,
        address,
        prefix_len,
        gateway,
        dns,
    };
    serialize_and_respond(connection, scratch, &status, extra_headers).await
}

/// Write a 400 Bad Request whose JSON body is `message`.
///
/// Serializes a [`GenericResponse`] into `scratch`; if it does not fit,
/// `fallback` supplies pre-encoded bytes so the client still gets a
/// well-formed error.
async fn write_bad_request<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    message: &str,
    fallback: &[u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let error = GenericResponse { message };
    let Ok(length) = serde_json_core::to_slice(&error, scratch) else {
        return write_json_response(connection, 400, "Bad Request", fallback, extra_headers).await;
    };
    write_json_response(
        connection,
        400,
        "Bad Request",
        &scratch[..length],
        extra_headers,
    )
    .await
}

/// Request body for `POST /api/system/network`.
///
/// Mirrors [`ubos_config::NetworkConfig`] field by field (`serde_json_core`
/// does not support `#[serde(flatten)]`) and adds `confirm_after_switch`.
/// The caller always sends this field; it is not optional.
#[derive(serde::Deserialize)]
struct NetworkPostRequest {
    /// Network mode to apply (DHCP or static).
    mode: ubos_config::NetworkMode,
    /// Desired hostname for the device on the network.
    hostname: heapless::String<{ ubos_config::network::HOSTNAME_MAX }>,
    /// When `true`, arm the 120 s confirm-or-revert window. When `false`,
    /// apply the change live and persist it immediately with no revert.
    confirm_after_switch: bool,
}

/// Handle `POST /api/system/network`.
///
/// Deserializes the body as a [`NetworkPostRequest`], validates the
/// embedded [`ubos_config::NetworkConfig`], and submits it to
/// [`crate::network_apply::apply`] for live application. When
/// `confirm_after_switch` is `true`, a 120 s confirm-or-revert window
/// is armed; when `false`, the change is persisted immediately with no
/// window. Returns 400 on parse or validation error, 200 on success.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_network_post<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let Ok((request, _)) = serde_json_core::de::from_slice::<NetworkPostRequest>(body) else {
        return write_bad_request(
            connection,
            scratch,
            "invalid JSON",
            b"{\"message\":\"invalid JSON\"}",
            extra_headers,
        )
        .await;
    };
    let network = ubos_config::NetworkConfig {
        mode: request.mode,
        hostname: request.hostname,
    };
    if let Err(message) = network.validate() {
        return write_bad_request(
            connection,
            scratch,
            message,
            b"{\"message\":\"validation error\"}",
            extra_headers,
        )
        .await;
    }
    // A wrong or unreachable static address strands the board, and applying
    // with no confirm window persists it immediately, so a reboot cannot
    // recover. Require the confirm-or-revert window for static changes; DHCP
    // self-heals on reboot, so it may still opt out.
    if matches!(network.mode, ubos_config::NetworkMode::Static { .. })
        && !request.confirm_after_switch
    {
        return write_bad_request(
            connection,
            scratch,
            "static address changes require the confirm window",
            b"{\"message\":\"static address changes require the confirm window\"}",
            extra_headers,
        )
        .await;
    }
    if !crate::network_apply::apply(network, request.confirm_after_switch) {
        return write_busy(connection, extra_headers).await;
    }
    write_json_response(connection, 200, "OK", b"{\"ok\":true}", extra_headers).await
}

/// Write a 503 Service Unavailable when the apply queue is full.
///
/// The apply task drains a bounded channel; a burst can momentarily fill
/// it. Reporting 503 lets the operator retry instead of seeing a success
/// that never took effect.
async fn write_busy<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    write_json_response(
        connection,
        503,
        "Service Unavailable",
        b"{\"message\":\"busy, retry\"}",
        extra_headers,
    )
    .await
}

/// Handle `POST /api/system/network/confirm`.
///
/// Calls [`crate::network_apply::confirm`] and returns 200 ack.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_network_confirm<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if !crate::network_apply::confirm() {
        return write_busy(connection, extra_headers).await;
    }
    write_ack_response(connection, scratch, extra_headers).await
}

/// Handle a POST action endpoint (restart, identify,
/// blockFound/dismiss).
///
/// Logs one `info!` line naming the action and returns 200 with
/// `{"message":"ack"}`. Pause and resume are handled separately
/// by `dispatch` because they carry a `Manager` side-effect.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_action<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    action: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    info!("api action: {}", action);
    write_ack_response(connection, scratch, extra_headers).await
}

/// Handle `POST /api/system/factory_reset`.
///
/// Sends [`ubos_config::Message::FactoryReset`] to the config task and
/// writes the 200 ack. The config task wipes persisted config back to
/// defaults and only then requests the reboot, so the flash erase and
/// write always complete before the reset task's flush delay elapses and
/// the MCU resets. The ack reaches the wire well ahead of that reset.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_factory_reset<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ubos_config::handle()
        .send(ubos_config::Message::FactoryReset)
        .await;
    write_ack_response(connection, scratch, extra_headers).await
}

/// Handle `PATCH /api/system`.
///
/// Logs the body byte count and returns 200 with `{"message":"ack"}`.
/// No fields are applied; the audit doc places credential and config
/// persistence out of scope for this phase. The body itself is not
/// logged because legitimate clients may include credentials in the
/// JSON payload, and the stub has no operational use for the contents.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_patch_system<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    body_len: usize,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    info!("api patch /api/system ({} bytes)", body_len);
    write_ack_response(connection, scratch, extra_headers).await
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
    /// `GET /api/system/thermal`
    SystemThermal,
    /// `GET /api/system/cooling`
    SystemCooling,
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
    /// `POST /api/system/factory_reset`
    FactoryReset,
    /// `GET /api/system/network`
    NetworkGet,
    /// `POST /api/system/network`
    NetworkPost,
    /// `POST /api/system/network/confirm`
    NetworkConfirm,
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
        "/api/system/thermal" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemThermal),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/cooling" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemCooling),
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
        "/api/system/factory_reset" => match method {
            "POST" => DispatchOutcome::Handled(Route::FactoryReset),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system" => match method {
            "PATCH" => DispatchOutcome::Handled(Route::PatchSystem),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "PATCH" },
        },
        "/api/system/network/confirm" => match method {
            "POST" => DispatchOutcome::Handled(Route::NetworkConfirm),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "POST" },
        },
        "/api/system/network" => match method {
            "GET" => DispatchOutcome::Handled(Route::NetworkGet),
            "POST" => DispatchOutcome::Handled(Route::NetworkPost),
            _ => DispatchOutcome::MethodNotAllowed {
                allowed: "GET, POST",
            },
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
/// `extra_headers` is forwarded to every response writer. The REST
/// listener passes its pre-built CORS slice when the request had an
/// Origin header, and `&[]` otherwise.
///
/// Crate-private because the auth/CSRF/allow-list gates live in
/// `RestApiService::handle`; exposing this directly would let an
/// outside caller bypass those checks.
// The argument count comes from threading `extra_headers` and `stack`
// through the existing board_name/thermal/manager set. A context struct
// would reduce the count but add indirection without simplifying the callers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    method: &str,
    path: &str,
    body: &[u8],
    board_name: &str,
    thermal: ThermalHandle,
    manager: ManagerHandle,
    stack: Stack<'static>,
    extra_headers: &[(&str, &str)],
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
            write_method_not_allowed(connection, allowed, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemInfo) => {
            handle_info(connection, scratch, board_name, thermal, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemAsic) => {
            handle_asic(connection, scratch, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemThermal) => {
            handle_thermal(connection, scratch, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemCooling) => {
            handle_cooling(connection, scratch, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemStatistics) => {
            handle_statistics(connection, scratch, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemScoreboard) => {
            handle_scoreboard(connection, scratch, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemLogs) => {
            handle_not_implemented(connection, scratch, "not implemented", extra_headers).await?;
        }
        DispatchOutcome::Handled(
            Route::SystemWifiScan | Route::SystemOta | Route::SystemOtaWww,
        ) => {
            handle_not_implemented(
                connection,
                scratch,
                "unsupported on this platform",
                extra_headers,
            )
            .await?;
        }
        DispatchOutcome::Handled(Route::PauseAck) => {
            manager.send(ManagerMessage::Stop).await;
            handle_action(connection, scratch, "pause", extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::ResumeAck) => {
            manager.send(ManagerMessage::Start).await;
            handle_action(connection, scratch, "resume", extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::RestartAck) => {
            handle_action(connection, scratch, "restart", extra_headers).await?;
            ubos_core::reboot::request_reboot();
        }
        DispatchOutcome::Handled(Route::IdentifyAck) => {
            handle_action(connection, scratch, "identify", extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::BlockFoundDismissAck) => {
            handle_action(connection, scratch, "blockFound/dismiss", extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::PatchSystem) => {
            handle_patch_system(connection, scratch, body.len(), extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::FactoryReset) => {
            handle_factory_reset(connection, scratch, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::NetworkGet) => {
            handle_network_get(connection, scratch, stack, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::NetworkPost) => {
            handle_network_post(connection, scratch, body, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::NetworkConfirm) => {
            handle_network_confirm(connection, scratch, extra_headers).await?;
        }
    }

    Ok(true)
}

#[cfg(test)]
#[path = "tests/system.rs"]
mod tests;
