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

//! REST endpoints under `/api/system/*`.
//!
//! Dispatched from `RestApiService::handle`. Each handler builds a
//! value of the appropriate
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
//! Network endpoints under `/api/system/network`:
//!
//! - `GET /api/system/network` — reports the current network mode,
//!   pending-change flag, confirm countdown, and live address.
//! - `POST /api/system/network` — applies a new
//!   [`ubos_config::NetworkConfig`] live, optionally arming a
//!   confirm-or-revert window.
//! - `POST /api/system/network/{confirm,revert}` — commit or roll back
//!   a pending network change.
//!
//! OTA endpoints:
//!
//! - `POST /api/system/OTA` — stream a firmware image into the
//!   inactive slot; reboots into the new slot on success.
//! - `POST /api/system/OTA/chunk` — upload one sector per request,
//!   acknowledged individually, using the `X-OTA-Offset` and
//!   `X-OTA-Final` headers to order and terminate the transfer.
//!
//! Both OTA endpoints are intercepted in `rest.rs` ahead of [`dispatch`]
//! because they stream their body straight from the connection. They
//! require an [`crate::ota::OtaTarget`] registered via
//! [`crate::ota::register_ota_target`]; boards that never register one
//! receive a 501.
//!
//! Method enforcement is strict: every known path responds with 405
//! when the wrong method is used.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use heapless::String;
use ubos_logging::{info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_metrics::hashrate::hashrate;
use ubos_thermal::Handle as ThermalHandle;

use crate::ota::{
    OtaError, record_chunk_written, record_error, record_rebooting, record_session_begin,
};
use crate::schema::{
    API_VARIANT, GenericResponse, SystemInfo, SystemNetwork, SystemScoreboardEntry,
    SystemStatistics, system_info::HwInfo, system_statistics::LABELS,
};

#[cfg(test)]
#[path = "tests/system.rs"]
mod tests;

/// Serialization scratch buffer size in bytes.
///
/// The largest schema struct is `SystemInfo`, which serializes to roughly
/// 950 bytes with all string fields at their maximums: a ~600-byte base
/// plus roughly 350 bytes for `hwInfo`, whose string fields (bootloader
/// and fan-controller versions, PSU model, three hashboard serials) each
/// hold up to `HW_STRING_LEN` characters. 2048 bytes covers that worst
/// case and leaves headroom for fields added in later phases without
/// revisiting this constant.
///
/// Per-connection stack cost is `SCRATCH_SIZE` bytes; this multiplies by
/// `ControlBoardDescriptor::http_pool` for concurrent connections. If you
/// increase this constant, audit task-stack sizing for all ubos-api
/// consumers.
const SCRATCH_SIZE: usize = 2048;

/// OTA upload chunk size in bytes.
///
/// Each `write_chunk` call receives at most this many bytes. Sized to
/// one RP2350 flash sector (4096 bytes) so the implementation can flush
/// aligned sectors without buffering. Between chunks the handler pauses
/// briefly to drain the network and feed the watchdog.
///
/// Public so the RP flash target can statically assert it equals the
/// flash erase size, the invariant its per-chunk updater relies on.
pub const OTA_CHUNK_SIZE: usize = 4096;

/// Delay between sector writes during an OTA upload, in milliseconds.
///
/// The flash write pauses both cores; this pause afterwards gives the
/// network task time to drain the W6100's 4 KiB RX buffer, send ACKs, and
/// reopen the TCP window before the next read, so the chip buffer does not
/// overflow across freezes. Small enough to add negligible total time over
/// the ~150 sectors of a full image.
const OTA_DRAIN_DELAY_MS: u64 = 5;

/// Default maximum firmware image size accepted by the OTA upload handler.
///
/// Corresponds to the RP ACTIVE slot capacity the image must swap into:
/// the bootloader splits the flash into a 952 KiB ACTIVE slot and a
/// slightly larger DFU slot. An image larger than this is rejected up
/// front because it could never swap into ACTIVE even though the larger
/// DFU slot might hold it during upload.
///
/// This is the default returned by [`crate::ota::OtaTarget::max_image_bytes`];
/// a board with a larger inactive slot overrides that method and the
/// handlers honour the per-target value (see [`max_image_bytes`]). Must
/// stay in sync with `OTA_MAX_DEFAULT` in
/// `ubos-webui/web/components/ConfigSystem.tsx` and the `slotMaxBytes`
/// guard in `flake.nix`.
pub(crate) const OTA_MAX_IMAGE_BYTES: u64 = 952 * 1024;

/// Minimum firmware image size accepted by the OTA upload handler.
///
/// A Cortex-M vector table occupies the first 512 bytes of a valid
/// firmware image. Any image smaller than this cannot be valid firmware.
const OTA_MIN_IMAGE_BYTES: u64 = 512;

/// Maximum image size the registered target accepts.
///
/// Reads the board-reported cap from the lock-free OTA info snapshot,
/// falling back to [`OTA_MAX_IMAGE_BYTES`] when no target is registered (a
/// board without one returns 501 to the upload regardless, so this only
/// gates the pre-lock size check). Lets a board with a larger inactive slot
/// accept images above the RP default without a shared constant bump.
fn max_image_bytes() -> u64 {
    crate::ota::ota_info().map_or(OTA_MAX_IMAGE_BYTES, |info| info.max_image_bytes)
}

/// Build the dotted-quad IPv4 string from the shared net state.
///
/// Returns an empty string when DHCP has not yet assigned an address.
fn build_ip_string() -> String<16> {
    let mut result = String::new();
    if let Some(octets) = ubos_net::net::handle()
        .current()
        .and_then(|state| state.ipv4)
        .map(|ip| ip.octets())
    {
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
    let ntp_offset = ubos_net::net::handle().ntp_epoch_offset();
    if ntp_offset != 0 {
        let unix_secs = ubos_net::wall_clock_secs();
        let time = ubos_net::ntp::format_utc_time(unix_secs);
        let _ = result.push_str(time.as_str());
    }
    result
}

/// Maximum number of response headers built per system dispatch response.
///
/// Three fixed headers (Content-Type, Cache-Control, Content-Length)
/// plus up to two CORS headers from the REST listener equals five.
/// Eight slots leave headroom for future additions.
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
    let content_length = ubos_http::format_content_length(body.len());

    let mut headers: heapless::Vec<(&str, &str), MAX_SYSTEM_RESPONSE_HEADERS> =
        heapless::Vec::new();
    headers
        .push(("Content-Type", "application/json"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Cache-Control", "no-store"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", content_length.as_str()))
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
    let content_length = ubos_http::format_content_length(body.len());

    let mut headers: heapless::Vec<(&str, &str), MAX_SYSTEM_RESPONSE_HEADERS> =
        heapless::Vec::new();
    headers
        .push(("Allow", allowed))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Type", "text/plain"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Content-Length", content_length.as_str()))
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
/// When `ota` is `Some`, the `ota` field in the response carries the
/// running-slot label, running version, and previous-image information.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_info<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    board_name: &'static str,
    psu: ubos_psu::Handle,
    mining_info: &'static ubos_config::MiningInfo,
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
    let fan = ubos_cooling::handle().current();
    let fan_speed_rpm = fan.map_or(0, |f| f.rpm.rpm());
    let fan_pwm_percent = fan.map_or(0, |f| f.pwm.percent());
    let pools_bitmap = stratum_client::pool_alive_bitmap();
    let shares_accepted = stratum_client::shares_accepted();
    let shares_rejected = stratum_client::shares_rejected();
    let shares_total = ubos_metrics::hashrate::share_count();
    // Extract the configured hostname before building SystemInfo so the
    // full Config snapshot is dropped before the serialize_and_respond
    // await, keeping the response future small.
    let configured_hostname = ubos_config::handle().current().network.hostname;
    let hostname = if configured_hostname.is_empty() {
        board_name
    } else {
        configured_hostname.as_str()
    };

    // Lock-free read of the published OTA snapshot. Unlike taking the
    // upload mutex, this stays readable while an upload is in progress.
    let ota_info = crate::ota::ota_info();

    let pause_reasons = ubos_manager::handle().pause_reasons();
    let paused = !pause_reasons.is_empty();
    let pause_reason = pause_reasons
        .dominant()
        .map(ubos_manager::PauseReason::as_api_str);

    let hw_info = HwInfo {
        bootloader_version: crate::hw_info::bootloader_version(),
        fan_controller_version: crate::hw_info::fan_controller_version(),
        psu_model: psu.model(),
        hashboard_ids: crate::hw_info::hashboard_ids(),
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
        asic_count: mining_info.asic_count,
        asic_model: mining_info.asic_model,
        default_frequency_mhz: mining_info.default_frequency_mhz,
        default_voltage_mv: mining_info.default_voltage_mv,
        shares_accepted,
        shares_rejected,
        shares_total,
        best_nonce_diff: 0,
        best_session_nonce_diff: 0,
        ota: ota_info,
        paused,
        pause_reason,
        hw_info,
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
    mining_info: &'static ubos_config::MiningInfo,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(connection, scratch, mining_info, extra_headers).await
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
    let ntp_offset = ubos_net::net::handle().ntp_epoch_offset();
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
    let seconds_remaining = ubos_config::pending_seconds_remaining();
    // While a change is pending the persisted mode still reflects the old
    // config, but the live address already reflects the preview. Report the
    // previewed mode so the two agree until the operator confirms or it
    // reverts.
    let mode = match ubos_config::pending_mode_is_static() {
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
/// embedded [`ubos_config::NetworkConfig`], and forwards it to
/// `ubos-config` as a [`ubos_config::Message::ApplyNetwork`]; `ubos-net`
/// switches the live stack on the resulting config change. When
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
    if !ubos_config::handle().try_send(ubos_config::Message::ApplyNetwork {
        network,
        confirm: request.confirm_after_switch,
    }) {
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
/// Forwards [`ubos_config::Message::ConfirmNetwork`] and returns 200 ack.
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
    if !ubos_config::handle().try_send(ubos_config::Message::ConfirmNetwork) {
        return write_busy(connection, extra_headers).await;
    }
    write_ack_response(connection, scratch, extra_headers).await
}

/// Handle `POST /api/system/network/revert`.
///
/// Forwards [`ubos_config::Message::RevertNetwork`] and returns 200 ack.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_network_revert<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if !ubos_config::handle().try_send(ubos_config::Message::RevertNetwork) {
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
/// Clears all sessions, sends [`ubos_config::Message::FactoryReset`] to the
/// config task, and writes the 200 ack. The config task wipes persisted
/// config back to defaults and only then requests the reboot, so the flash
/// erase and write always complete before the reset task's flush delay
/// elapses and the MCU resets. The ack reaches the wire well ahead of that
/// reset. Sessions are cleared up front so the operator's cookie cannot
/// authenticate during the brief window before the reset fires, matching
/// the live network apply. `extra_headers` carries caller-supplied headers
/// such as CORS headers from the REST listener.
async fn handle_factory_reset<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ubos_auth::session::clear_all_sessions().await;
    ubos_config::handle()
        .send(ubos_config::Message::FactoryReset)
        .await;
    write_ack_response(connection, scratch, extra_headers).await
}

/// Handle `PATCH /api/system`.
///
/// Logs the body byte count and returns 200 with `{"message":"ack"}`.
/// No fields are applied; credential and config persistence is handled
/// by `POST /api/config`. The body itself is not
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

/// Handle `POST /api/system/OTA` — streaming firmware upload.
///
/// Streams the request body directly from the TCP connection into the
/// inactive OTA slot via `ota.write_chunk()`. The read loop consumes
/// exactly `content_length` bytes in [`OTA_CHUNK_SIZE`]-byte chunks; a
/// short [`OTA_DRAIN_DELAY_MS`] delay between sector writes lets the
/// network task drain the chip buffer and feed the watchdog. On success
/// the handler writes a 200 ack and then calls `ota.reboot_into_new()`,
/// which never returns.
///
/// Error responses:
/// - 501 — no `OtaTarget` supplied (board not capable).
/// - 411 — no `Content-Length` header.
/// - 413 — image larger than the target's max image size.
/// - 400 — image smaller than [`OTA_MIN_IMAGE_BYTES`].
/// - 409 — `begin()` returned [`OtaError::NoPartitionTable`].
/// - 503 — mining failed to stop within the timeout.
/// - 400 — premature EOF before `content_length` bytes were received.
/// - 500 — `write_chunk` or `finish` error.
///
/// `extra_headers` carries caller-supplied CORS headers.
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_ota<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    content_length: Option<u64>,
    manager: ManagerHandle,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    // Validate input before acquiring the mutex so a request rejected on
    // bad headers never holds the upload lock.
    let Some(image_bytes) = content_length else {
        return write_json_response(
            connection,
            411,
            "Length Required",
            b"{\"message\":\"Content-Length required for OTA upload\"}",
            extra_headers,
        )
        .await;
    };

    if image_bytes > max_image_bytes() {
        return write_json_response(
            connection,
            413,
            "Payload Too Large",
            b"{\"message\":\"image exceeds maximum slot size\"}",
            extra_headers,
        )
        .await;
    }

    if image_bytes < OTA_MIN_IMAGE_BYTES {
        // No session has started yet, so this is a client input rejection,
        // not a device error. Leaving the phase at Idle keeps /api/info from
        // reporting a stale Error until the next attempt or a reboot.
        return write_json_response(
            connection,
            400,
            "Bad Request",
            b"{\"message\":\"image too small\"}",
            extra_headers,
        )
        .await;
    }

    // Acquire the registered target. `try_lock` (not `lock`) so a second
    // concurrent upload is rejected rather than queued behind a write that
    // ends in a reboot.
    let Ok(mut guard) = crate::ota::target_mutex().try_lock() else {
        return write_json_response(
            connection,
            409,
            "Conflict",
            b"{\"message\":\"an update is already in progress\"}",
            extra_headers,
        )
        .await;
    };
    let Some(ota) = guard.as_deref_mut() else {
        return write_json_response(
            connection,
            501,
            "Not Implemented",
            b"{\"message\":\"OTA not available on this board\"}",
            extra_headers,
        )
        .await;
    };

    // `image_bytes` is now at most the target's max image size, which fits
    // in usize on all targets (any real slot is well under 4 GiB).
    #[allow(clippy::cast_possible_truncation)]
    let total_bytes = image_bytes as usize;

    // Mining must be quiesced before any flash access: the flash bus is
    // shared and the mining serial task runs on core 1, so an in-flight
    // UART DMA transfer can race a flash write if mining is still active.
    if !manager.stop_mining_and_wait().await {
        warn!("OTA: mining did not stop within timeout; aborting");
        // Mining may still stop after the timeout, so clear the pause.
        manager.send(ManagerMessage::OtaEnd).await;
        return write_json_response(
            connection,
            503,
            "Service Unavailable",
            b"{\"message\":\"mining did not stop in time; retry\"}",
            extra_headers,
        )
        .await;
    }

    match ota.begin() {
        Ok(()) => {
            record_session_begin();
        }
        Err(OtaError::NoPartitionTable) => {
            // Resume mining so the board keeps hashing after a failed update.
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                409,
                "Conflict",
                b"{\"message\":\"device not provisioned for OTA\"}",
                extra_headers,
            )
            .await;
        }
        Err(error) => {
            warn!("OTA begin failed: {:?}", error);
            record_error(error);
            // Resume mining so the board keeps hashing after a failed update.
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                500,
                "Internal Server Error",
                b"{\"message\":\"OTA begin failed\"}",
                extra_headers,
            )
            .await;
        }
    }

    // This 4096-byte buffer contributes to the task future size; see the
    // SCRATCH_SIZE note for guidance on auditing stack costs when adjusting.
    let mut chunk_buffer = [0u8; OTA_CHUNK_SIZE];
    let mut bytes_written = 0usize;

    while bytes_written < total_bytes {
        let want = (total_bytes - bytes_written).min(OTA_CHUNK_SIZE);
        let mut chunk_read = 0usize;

        // Fill the chunk buffer before calling write_chunk. Chunks are
        // OTA_CHUNK_SIZE bytes except the final chunk, which carries the
        // remainder; the target accepts a sub-sector final write.
        while chunk_read < want {
            match embedded_io_async::Read::read(connection, &mut chunk_buffer[chunk_read..want])
                .await
            {
                Ok(0) => {
                    // Premature EOF: the client closed before sending all bytes.
                    warn!(
                        "OTA: premature EOF at {}/{} bytes",
                        bytes_written + chunk_read,
                        total_bytes,
                    );
                    record_error(OtaError::ShortUpload);
                    ota.abort();
                    // Resume mining so the board keeps hashing after a failed update.
                    manager.send(ManagerMessage::OtaEnd).await;
                    return write_json_response(
                        connection,
                        400,
                        "Bad Request",
                        b"{\"message\":\"upload truncated\"}",
                        extra_headers,
                    )
                    .await;
                }
                Ok(read) => chunk_read += read,
                Err(_) => {
                    warn!("OTA: read error after {} bytes", bytes_written);
                    record_error(OtaError::Flash);
                    ota.abort();
                    // Resume mining so the board keeps hashing after a failed update.
                    manager.send(ManagerMessage::OtaEnd).await;
                    return write_json_response(
                        connection,
                        400,
                        "Bad Request",
                        b"{\"message\":\"read error during upload\"}",
                        extra_headers,
                    )
                    .await;
                }
            }
        }

        if let Err(error) = ota.write_chunk(&chunk_buffer[..chunk_read]) {
            warn!("OTA write_chunk failed: {:?}", error);
            record_error(error);
            ota.abort();
            // Resume mining so the board keeps hashing after a failed update.
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                500,
                "Internal Server Error",
                b"{\"message\":\"OTA write failed\"}",
                extra_headers,
            )
            .await;
        }
        bytes_written += chunk_read;
        #[allow(clippy::cast_possible_truncation)]
        record_chunk_written(bytes_written as u32);
        // Pause briefly between sector writes rather than yielding once. The
        // flash write pauses both cores, and the W6100 cannot be drained while
        // frozen, so a single yield is not enough for the network task to empty
        // the chip's RX buffer, send ACKs, and reopen the TCP window before the
        // next read. A short delay gives the executor several poll rounds to
        // drain the W6100 and feed the watchdog, keeping the chip's 4 KiB
        // buffer from accumulating across freezes.
        Timer::after(Duration::from_millis(OTA_DRAIN_DELAY_MS)).await;
    }

    if let Err(error) = ota.finish() {
        warn!("OTA finish failed: {:?}", error);
        record_error(error);
        ota.abort();
        // Resume mining so the board keeps hashing after a failed update.
        manager.send(ManagerMessage::OtaEnd).await;
        return write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"message\":\"OTA finish failed\"}",
            extra_headers,
        )
        .await;
    }

    info!(
        "OTA upload complete ({} bytes); rebooting into new slot",
        total_bytes
    );
    record_rebooting();
    // The image is staged and finish() marked the swap, so commit it by
    // rebooting regardless of whether the ack reaches the client. A failed
    // ack write must not skip the reboot: that would leave mining stopped and
    // the swap pending. The reboot restarts mining on the new image, so a lost
    // ack is harmless and matches the client's expected connection drop.
    let _ = write_json_response(connection, 200, "OK", b"{\"ok\":true}", extra_headers).await;

    ota.reboot_into_new()
}

/// Handle `POST /api/system/OTA/chunk` — single-chunk firmware upload.
///
/// Receives one sector-sized body (at most [`OTA_CHUNK_SIZE`] bytes) and
/// writes it to the inactive slot. The client sends chunks one at a time,
/// waiting for the 200 ack before sending the next, so no bytes are
/// in-flight during the flash write and the W6100's 4 KiB RX buffer
/// cannot overflow.
///
/// Required headers:
/// - `X-OTA-Offset` — decimal byte offset where this chunk begins.
/// - `X-OTA-Final` — present and equal to `"1"` on the last chunk only.
/// - `Content-Length` — body byte count (required, max [`OTA_CHUNK_SIZE`]).
///
/// Session management:
/// - `X-OTA-Offset: 0` starts a new session: mining is stopped and
///   `begin()` is called. Any in-progress session is implicitly abandoned
///   (the target's `begin()` re-arms at offset 0).
/// - Any other offset continues an existing session. The offset must equal
///   the number of bytes already written; a mismatch returns 409.
///
/// Mining stays stopped between chunks and is resumed only on an error or
/// via the reboot triggered by the final chunk.
///
/// Error responses:
/// - 501 — no `OtaTarget` supplied (board not capable).
/// - 409 — target locked by a concurrent upload, or offset mismatch.
/// - 411 — `Content-Length` absent.
/// - 413 — chunk larger than [`OTA_CHUNK_SIZE`].
/// - 503 — mining did not stop within the timeout (offset-0 only).
/// - 400 — premature EOF, read error, or total image smaller than [`OTA_MIN_IMAGE_BYTES`] on final chunk.
/// - 500 — `begin`, `write_chunk`, or `finish` error.
///
/// Success responses:
/// - Mid-stream: 200 `{"ok":true,"next":<offset+n>}`.
/// - Final chunk: 200 `{"ok":true}` then the board reboots.
///
/// `ota_offset` is the parsed value of `X-OTA-Offset`.
/// `ota_final` is `true` when `X-OTA-Final` equals `"1"`.
/// `extra_headers` carries caller-supplied CORS headers.
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_ota_chunk<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    content_length: Option<u64>,
    ota_offset: u64,
    ota_final: bool,
    manager: ManagerHandle,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    // Validate input before acquiring the mutex so a request rejected on
    // bad headers never holds the upload lock.
    let Some(chunk_bytes) = content_length else {
        return write_json_response(
            connection,
            411,
            "Length Required",
            b"{\"message\":\"Content-Length required for OTA chunk\"}",
            extra_headers,
        )
        .await;
    };

    if chunk_bytes > OTA_CHUNK_SIZE as u64 {
        return write_json_response(
            connection,
            413,
            "Payload Too Large",
            b"{\"message\":\"chunk exceeds maximum sector size\"}",
            extra_headers,
        )
        .await;
    }

    if chunk_bytes == 0 {
        return write_json_response(
            connection,
            400,
            "Bad Request",
            b"{\"message\":\"empty chunk\"}",
            extra_headers,
        )
        .await;
    }

    if ota_offset.saturating_add(chunk_bytes) > max_image_bytes() {
        return write_json_response(
            connection,
            413,
            "Payload Too Large",
            b"{\"message\":\"image exceeds maximum slot size\"}",
            extra_headers,
        )
        .await;
    }

    // Reject concurrent uploads: a second connection finds the mutex held
    // and is refused rather than queued behind a write that ends in a reboot.
    let Ok(mut guard) = crate::ota::target_mutex().try_lock() else {
        return write_json_response(
            connection,
            409,
            "Conflict",
            b"{\"message\":\"an update is already in progress\"}",
            extra_headers,
        )
        .await;
    };
    let Some(ota) = guard.as_deref_mut() else {
        return write_json_response(
            connection,
            501,
            "Not Implemented",
            b"{\"message\":\"OTA not available on this board\"}",
            extra_headers,
        )
        .await;
    };

    // `chunk_bytes` is at most OTA_CHUNK_SIZE which fits in usize on all targets.
    #[allow(clippy::cast_possible_truncation)]
    let chunk_size = chunk_bytes as usize;

    if ota_offset == 0 {
        // A fresh offset-0 chunk restarts the session. If a prior session was
        // still active, abort it first so its core-1 park, breadcrumb, and
        // write offset are reset before re-arming the slot writer.
        if ota.bytes_written().is_some() {
            warn!("OTA chunk: offset 0 with active session; aborting prior session");
            ota.abort();
        }
        // Start of a new upload session: quiesce mining, then arm the slot writer.
        if !manager.stop_mining_and_wait().await {
            warn!("OTA chunk: mining did not stop within timeout; aborting");
            // Mining may still stop after the timeout, so clear the pause.
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                503,
                "Service Unavailable",
                b"{\"message\":\"mining did not stop in time; retry\"}",
                extra_headers,
            )
            .await;
        }

        match ota.begin() {
            Ok(()) => {
                record_session_begin();
            }
            Err(OtaError::NoPartitionTable) => {
                manager.send(ManagerMessage::OtaEnd).await;
                return write_json_response(
                    connection,
                    409,
                    "Conflict",
                    b"{\"message\":\"device not provisioned for OTA\"}",
                    extra_headers,
                )
                .await;
            }
            Err(error) => {
                warn!("OTA chunk begin failed: {:?}", error);
                record_error(error);
                manager.send(ManagerMessage::OtaEnd).await;
                return write_json_response(
                    connection,
                    500,
                    "Internal Server Error",
                    b"{\"message\":\"OTA begin failed\"}",
                    extra_headers,
                )
                .await;
            }
        }
    } else {
        // Continuation: the incoming offset must equal the bytes already written.
        // A mismatch means an out-of-order or replayed request.
        let expected = ota.bytes_written().unwrap_or(0);
        // `ota_offset` is a u64 header value; the target write position fits in
        // usize on all supported targets (max image is ~1 MiB << 4 GiB).
        #[allow(clippy::cast_possible_truncation)]
        let ota_offset_usize = ota_offset as usize;
        if ota_offset_usize != expected {
            warn!(
                "OTA chunk: offset mismatch: got {} expected {}",
                ota_offset_usize, expected,
            );
            ota.abort();
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                409,
                "Conflict",
                b"{\"message\":\"chunk offset does not match write position; restart from offset 0\"}",
                extra_headers,
            )
            .await;
        }
    }

    // Every chunk but the last must cover exactly one erase sector.
    // write_chunk rebuilds a fresh updater per call and assumes one chunk
    // maps to one sector; a short non-final chunk would leave write_offset
    // off a sector boundary and let the next chunk re-erase a written sector.
    // The session is armed by this point, so abort it before rejecting.
    if !ota_final && chunk_size != OTA_CHUNK_SIZE {
        warn!(
            "OTA chunk: non-final chunk is {} bytes, not a full sector",
            chunk_size,
        );
        ota.abort();
        manager.send(ManagerMessage::OtaEnd).await;
        return write_json_response(
            connection,
            400,
            "Bad Request",
            b"{\"message\":\"non-final chunk must be exactly one sector\"}",
            extra_headers,
        )
        .await;
    }

    // Read the chunk body fully before calling write_chunk. The buffer is
    // sized to OTA_CHUNK_SIZE so the largest legal chunk fits without
    // reallocation.
    let mut chunk_buffer = [0u8; OTA_CHUNK_SIZE];
    let mut chunk_read = 0usize;

    while chunk_read < chunk_size {
        match embedded_io_async::Read::read(connection, &mut chunk_buffer[chunk_read..chunk_size])
            .await
        {
            Ok(0) => {
                warn!(
                    "OTA chunk: premature EOF at {}/{} bytes",
                    chunk_read, chunk_size,
                );
                record_error(OtaError::ShortUpload);
                ota.abort();
                manager.send(ManagerMessage::OtaEnd).await;
                return write_json_response(
                    connection,
                    400,
                    "Bad Request",
                    b"{\"message\":\"chunk upload truncated\"}",
                    extra_headers,
                )
                .await;
            }
            Ok(read) => chunk_read += read,
            Err(_) => {
                warn!("OTA chunk: read error after {} bytes", chunk_read);
                record_error(OtaError::Flash);
                ota.abort();
                manager.send(ManagerMessage::OtaEnd).await;
                return write_json_response(
                    connection,
                    400,
                    "Bad Request",
                    b"{\"message\":\"read error during chunk upload\"}",
                    extra_headers,
                )
                .await;
            }
        }
    }

    if let Err(error) = ota.write_chunk(&chunk_buffer[..chunk_read]) {
        warn!("OTA chunk write_chunk failed: {:?}", error);
        record_error(error);
        ota.abort();
        manager.send(ManagerMessage::OtaEnd).await;
        return write_json_response(
            connection,
            500,
            "Internal Server Error",
            b"{\"message\":\"OTA write failed\"}",
            extra_headers,
        )
        .await;
    }

    // `ota_offset` + `chunk_read` is the number of bytes written so far.
    // Both are bounded by the target's max image size (fits in u32 on all targets).
    #[allow(clippy::cast_possible_truncation)]
    record_chunk_written((ota_offset as u32).saturating_add(chunk_read as u32));

    if ota_final {
        // `ota_offset` + `chunk_read` is the total image size delivered.
        // Both are bounded by the target's max image size (fits in u64 on all targets).
        let total_image_bytes = ota_offset.saturating_add(chunk_read as u64);
        if total_image_bytes < OTA_MIN_IMAGE_BYTES {
            record_error(OtaError::ShortUpload);
            ota.abort();
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                400,
                "Bad Request",
                b"{\"message\":\"image too small\"}",
                extra_headers,
            )
            .await;
        }

        if let Err(error) = ota.finish() {
            warn!("OTA chunk finish failed: {:?}", error);
            record_error(error);
            ota.abort();
            manager.send(ManagerMessage::OtaEnd).await;
            return write_json_response(
                connection,
                500,
                "Internal Server Error",
                b"{\"message\":\"OTA finish failed\"}",
                extra_headers,
            )
            .await;
        }

        // `ota_offset` is bounded by the target's max image size (fits in usize on all targets).
        #[allow(clippy::cast_possible_truncation)]
        let total_bytes = ota_offset as usize + chunk_read;
        info!(
            "OTA chunk upload complete ({} bytes); rebooting into new slot",
            total_bytes,
        );
        record_rebooting();
        // The image is staged and finish() marked the swap. Commit it by
        // rebooting regardless of whether the ack reaches the client; a
        // lost ack matches the expected connection drop on reset, and a
        // failed ack must not skip the reboot or leave mining stopped.
        let _ = write_json_response(connection, 200, "OK", b"{\"ok\":true}", extra_headers).await;
        ota.reboot_into_new()
    } else {
        // Acknowledge the chunk and tell the client the next expected offset.
        // `ota_offset` is bounded by the target's max image size (fits in usize on all targets).
        #[allow(clippy::cast_possible_truncation)]
        let next_offset = ota_offset as usize + chunk_read;
        let mut ack_buf = [0u8; 40];
        let mut writer = ubos_http::BufferWriter::new(&mut ack_buf);
        // Safety: the format args are plain integers, so write! cannot fail.
        let _ = core::write!(writer, "{{\"ok\":true,\"next\":{next_offset}}}");
        let ack_len = writer.len();
        write_json_response(connection, 200, "OK", &ack_buf[..ack_len], extra_headers).await
    }
}

/// Begin a streaming JSON response with no `Content-Length` header.
///
/// edge-http upgrades a response that declares no `Content-Length` to
/// chunked transfer encoding on a keep-alive HTTP/1.1 connection. That
/// framing accepts multiple subsequent writes, each sent as its own chunk.
/// Callers write the body in pieces after this function returns.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn begin_streamed_json<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut headers: heapless::Vec<(&str, &str), MAX_SYSTEM_RESPONSE_HEADERS> =
        heapless::Vec::new();
    headers
        .push(("Content-Type", "application/json"))
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

/// Map a severity byte to its JSON string representation.
///
/// Values follow the `Severity` repr: 1 is warn, 2 is error,
/// all others (including 0) are info.
fn severity_str(value: u8) -> &'static str {
    match value {
        1 => "warn",
        2 => "error",
        _ => "info",
    }
}

/// Map an `ArgWidth` variant to its JSON string representation.
fn argwidth_str(width: ubos_events::ArgWidth) -> &'static str {
    match width {
        ubos_events::ArgWidth::U8 => "u8",
        ubos_events::ArgWidth::U16 => "u16",
        ubos_events::ArgWidth::U32 => "u32",
        ubos_events::ArgWidth::I8 => "i8",
        ubos_events::ArgWidth::I16 => "i16",
        ubos_events::ArgWidth::I32 => "i32",
    }
}

/// Parse the `since` query parameter from a path+query string.
///
/// Returns the numeric value of `?since=<u32>`. A value that overflows
/// `u32` clamps to `u32::MAX`, so an oversized cursor returns no records
/// rather than the whole ring. An absent parameter or any other
/// unparseable value returns zero.
fn parse_since(path: &str) -> u32 {
    let Some((_, query)) = path.split_once('?') else {
        return 0;
    };
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("since=") {
            return match value.parse::<u32>() {
                Ok(seq) => seq,
                Err(error) if *error.kind() == core::num::IntErrorKind::PosOverflow => u32::MAX,
                Err(_) => 0,
            };
        }
    }
    0
}

/// Handle `GET /api/system/events`.
///
/// Streams the event ring as a JSON object `{"events":[...]}` with records
/// in oldest-first order. An optional `?since=<seq>` query parameter returns
/// only records whose `seq` is greater than `since`, so callers poll for new
/// records. `extra_headers` carries caller-supplied headers such as CORS
/// headers from the REST listener.
///
/// The ring is read in small batches advanced by a `seq` cursor rather than
/// copied whole, so no buffer scales with the ring capacity. The read is
/// bounded to the records present when it began by capturing the latest `seq`
/// up front; records recorded mid-stream are left for a later poll.
///
/// If the ring wraps past the caller's `since` between polls the skipped
/// records are gone: the response resumes at the oldest live record, so the
/// `seq` numbers jump and the browser detects the gap.
async fn handle_events<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let latest_seq = ubos_events::latest_seq();
    let mut cursor = parse_since(path);

    begin_streamed_json(connection, extra_headers).await?;
    embedded_io_async::Write::write_all(connection, b"{\"events\":[").await?;

    let mut first_emitted = true;
    let mut batch = [ubos_events::EventRecord::EMPTY; EVENT_BATCH];
    while cursor < latest_seq {
        let count = ubos_events::snapshot_after(&mut batch, cursor);
        if count == 0 {
            break;
        }
        for record in &batch[..count] {
            let mut buffer = [0u8; EVENT_JSON_MAX];
            let written = {
                let mut writer = ubos_http::BufferWriter::new(&mut buffer);
                if !first_emitted {
                    let _ = core::write!(writer, ",");
                }
                let _ = core::write!(
                    writer,
                    "{{\"seq\":{},\"ts\":{},\"sev\":\"{}\",\"code\":{},\"args\":\"",
                    record.seq,
                    record.ts,
                    severity_str(record.severity),
                    record.code,
                );
                for byte in record.args {
                    let _ = core::write!(writer, "{byte:02x}");
                }
                let _ = core::write!(writer, "\"}}");
                if writer.overflowed() {
                    warn!(
                        "events: record seq {} exceeded {}B buffer; emitting truncated JSON",
                        record.seq, EVENT_JSON_MAX
                    );
                }
                writer.len()
            };
            embedded_io_async::Write::write_all(connection, &buffer[..written]).await?;
            first_emitted = false;
            cursor = record.seq;
        }
    }

    embedded_io_async::Write::write_all(connection, b"]}").await?;
    Ok(())
}

/// Handle `GET /api/system/events/catalog`.
///
/// Streams the assembled event catalog as a JSON array. Each element
/// carries the event `code`, `sev`, `fmt` string, and `args` width list.
/// Returns `[]` until firmware installs the catalogs via `set_catalogs`.
/// `extra_headers` carries caller-supplied headers such as CORS headers
/// from the REST listener.
async fn handle_events_catalog<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    extra_headers: &[(&str, &str)],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    begin_streamed_json(connection, extra_headers).await?;
    embedded_io_async::Write::write_all(connection, b"[").await?;

    let mut first_def = true;
    for catalog in ubos_events::catalogs() {
        for def in *catalog {
            let mut buffer = [0u8; DEF_JSON_MAX];
            let written = {
                let mut writer = ubos_http::BufferWriter::new(&mut buffer);
                if !first_def {
                    let _ = core::write!(writer, ",");
                }
                let _ = core::write!(
                    writer,
                    "{{\"code\":{},\"sev\":\"{}\",\"fmt\":\"",
                    def.code,
                    severity_str(def.severity as u8),
                );
                // Escape the format string: `"` becomes `\"`, `\` becomes `\\`.
                for ch in def.fmt.chars() {
                    match ch {
                        '"' => {
                            let _ = core::write!(writer, "\\\"");
                        }
                        '\\' => {
                            let _ = core::write!(writer, "\\\\");
                        }
                        other => {
                            let _ = core::write!(writer, "{other}");
                        }
                    }
                }
                let _ = core::write!(writer, "\",\"args\":[");
                let mut first_arg = true;
                for arg_width in def.args {
                    if !first_arg {
                        let _ = core::write!(writer, ",");
                    }
                    let _ = core::write!(writer, "\"{}\"", argwidth_str(*arg_width));
                    first_arg = false;
                }
                let _ = core::write!(writer, "]}}");
                if writer.overflowed() {
                    warn!(
                        "events: catalog def code {} exceeded {}B buffer; emitting truncated JSON",
                        def.code, DEF_JSON_MAX
                    );
                }
                writer.len()
            };
            embedded_io_async::Write::write_all(connection, &buffer[..written]).await?;
            first_def = false;
        }
    }

    embedded_io_async::Write::write_all(connection, b"]").await?;
    Ok(())
}

/// Records copied from the ring per streaming batch. Sized for a small stack
/// buffer rather than the full ring, so the events endpoint costs no RAM that
/// scales with `EVENT_LOG_CAPACITY` on any board.
const EVENT_BATCH: usize = 16;

/// Maximum byte length of one serialised event record in the streaming JSON body.
///
/// The longest record serialises, with a leading comma, as
/// `,{"seq":4294967295,"ts":4294967295,"sev":"error","code":4294967295,"args":"<22 hex chars>"}`,
/// which is 99 bytes. 128 leaves conservative headroom.
const EVENT_JSON_MAX: usize = 128;

/// Maximum byte length of one serialised event definition in the streaming JSON body.
///
/// The layout accommodates a code (10 digits), severity string (5 chars), a
/// format string of up to ~169 bytes after the fixed punctuation, and an args
/// array of up to six quoted width strings, all within 256 bytes.
const DEF_JSON_MAX: usize = 256;

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
    /// `GET /api/system/events`
    SystemEvents,
    /// `GET /api/system/events/catalog`
    SystemEventsCatalog,
    /// `GET /api/system/wifi/scan`
    SystemWifiScan,
    /// `POST /api/system/OTA`
    SystemOta,
    /// `POST /api/system/OTA/chunk`
    SystemOtaChunk,
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
    /// `POST /api/system/network/revert`
    NetworkRevert,
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
    // Strip any query string so routes always match on the path only.
    let path = path.split_once('?').map_or(path, |(prefix, _)| prefix);
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
        "/api/system/events/catalog" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemEventsCatalog),
            _ => DispatchOutcome::MethodNotAllowed { allowed: "GET" },
        },
        "/api/system/events" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemEvents),
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
        "/api/system/OTA/chunk" => match method {
            "POST" => DispatchOutcome::Handled(Route::SystemOtaChunk),
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
        "/api/system/network/revert" => match method {
            "POST" => DispatchOutcome::Handled(Route::NetworkRevert),
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
// through the existing board_name/psu/thermal/manager set. A context
// struct would reduce the count but add indirection without simplifying
// the callers.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) async fn dispatch<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    method: &str,
    path: &str,
    body: &[u8],
    board_name: &'static str,
    psu: ubos_psu::Handle,
    mining_info: &'static ubos_config::MiningInfo,
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
            handle_info(
                connection,
                scratch,
                board_name,
                psu,
                mining_info,
                thermal,
                extra_headers,
            )
            .await?;
        }
        DispatchOutcome::Handled(Route::SystemAsic) => {
            handle_asic(connection, scratch, mining_info, extra_headers).await?;
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
        DispatchOutcome::Handled(Route::SystemEvents) => {
            handle_events(connection, path, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemEventsCatalog) => {
            handle_events_catalog(connection, extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::SystemWifiScan | Route::SystemOtaWww) => {
            handle_not_implemented(
                connection,
                scratch,
                "unsupported on this platform",
                extra_headers,
            )
            .await?;
        }
        DispatchOutcome::Handled(Route::SystemOta) => {
            // The streaming upload is intercepted in rest.rs before the body
            // is pre-read, so this arm is not reached in normal operation;
            // respond defensively rather than mishandle the unread body.
            handle_not_implemented(
                connection,
                scratch,
                "OTA upload must use the streaming path",
                extra_headers,
            )
            .await?;
        }
        DispatchOutcome::Handled(Route::SystemOtaChunk) => {
            // The chunk upload is intercepted in rest.rs before the body is
            // pre-read (same as SystemOta), so this arm is not reached in
            // normal operation; respond defensively.
            handle_not_implemented(
                connection,
                scratch,
                "OTA chunk upload must use the streaming path",
                extra_headers,
            )
            .await?;
        }
        DispatchOutcome::Handled(Route::PauseAck) => {
            crate::events::user_paused();
            manager.send(ManagerMessage::Stop).await;
            handle_action(connection, scratch, "pause", extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::ResumeAck) => {
            crate::events::user_resumed();
            manager.send(ManagerMessage::Start).await;
            handle_action(connection, scratch, "resume", extra_headers).await?;
        }
        DispatchOutcome::Handled(Route::RestartAck) => {
            crate::events::user_restart();
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
        DispatchOutcome::Handled(Route::NetworkRevert) => {
            handle_network_revert(connection, scratch, extra_headers).await?;
        }
    }

    Ok(true)
}
