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

//! REST endpoints under `/api/system/*`, dispatched from
//! `RestApiService::handle`. The routes are the `Route` variants;
//! the surface is documented in `docs/REST_API.md`.
//!
//! The two OTA endpoints are intercepted in `rest.rs` ahead of
//! [`dispatch`], because they stream their body straight from the
//! connection. A known path with the wrong method answers 405.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use heapless::String;
use ubos_config::{Source, Statistic};
use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_http::{Status, start_response, write_json_response, write_message};
use ubos_logging::{info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_thermal::Handle as ThermalHandle;

use crate::ota::{OtaError, OtaTarget, ota_session};
use crate::rest::RestApiService;
use crate::schema::events::{EventDefJson, EventRecordJson};
use crate::schema::{
    API_VARIANT, SystemInfo, SystemNetwork, SystemScoreboardEntry, SystemStatistics,
    SystemTemperature, system_info::HwInfo, system_statistics::LABELS,
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
/// hold up to `HW_STRING_LEN` characters, and the build id.
/// The build id is written through a JSON string escaper, so its worst
/// case is twice `crate::ota::MAX_BUILD_ID_LEN` rather than that bound
/// itself: the build environment can hand the id whole with a quote or a
/// backslash in it. The brand and product names
/// come from a board's stored identity and can be all control
/// characters, each escaped to six bytes. 2048
/// bytes covers all of it and leaves headroom for fields added in later
/// phases without revisiting this constant.
///
/// `worst_case_system_info_fits_the_scratch_buffer` in `tests/system.rs`
/// holds that claim, so a field added to this body fails a test rather
/// than silently eating the headroom.
///
/// Per-connection stack cost is `SCRATCH_SIZE` bytes; this multiplies by
/// `HttpPoolSizes::http` for concurrent connections. If you
/// increase this constant, audit task-stack sizing for all ubos-api
/// consumers.
const SCRATCH_SIZE: usize = 2048;

/// OTA upload chunk size: one RP flash sector, so the target flushes
/// aligned sectors without buffering. Public so the RP target can assert
/// it equals the erase size, which its per-chunk updater relies on.
pub const OTA_CHUNK_SIZE: usize = 4096;

/// Delay between sector writes in a streaming OTA upload. The flash write
/// pauses both cores, and the chip's RX buffer cannot be drained while
/// frozen, so one yield is not enough for the network task to empty it
/// and reopen the TCP window before the next read.
const OTA_DRAIN_DELAY_MS: u64 = 5;

/// Default largest image the OTA handlers accept: the RP ACTIVE slot the
/// image must swap into, which is smaller than the DFU slot that holds
/// it during upload. Must stay in sync with `OTA_MAX_DEFAULT` in
/// `ubos-webui/web/components/ConfigSystem.tsx` and the `slotMaxBytes`
/// guard in `flake.nix`.
pub(crate) const OTA_MAX_IMAGE_BYTES: u64 = 952 * 1024;

/// Smallest image the OTA handlers accept: a Cortex-M vector table alone
/// is this long.
const OTA_MIN_IMAGE_BYTES: u64 = 512;

/// How long an upload waits for the miner to confirm that it stopped. The
/// manager halts mining within one scheduler round-trip, so the bound only
/// stops a wedged miner from hanging the request.
const MINING_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Assert the OTA pause and wait for the miner to stop, which an upload does
/// before it touches flash. Returns `false` if the miner does not stop in
/// [`MINING_STOP_TIMEOUT`].
async fn quiesce_mining(manager: ManagerHandle, mining: ubos_mining::Handle) -> bool {
    manager.send(ManagerMessage::OtaBegin).await;
    mining.wait_stopped(MINING_STOP_TIMEOUT).await
}

/// Downgrade the config for the image that `ota` staged, and wait until
/// the config task has stored the record that the image reads.
///
/// Called after `finish` accepted the image and before the reboot, so
/// the image boots on a record it reads. A target that cannot tell what
/// the image reads downgrades nothing.
async fn downgrade_config(ota: &dyn crate::ota::OtaTarget, config: ubos_config::Handle) {
    let Some(staged) = ota.staged_config() else {
        info!(
            "OTA: the config that the image reads is not known, rebooting without the config downgrade"
        );
        return;
    };
    if !config.downgrade(staged).await {
        warn!("OTA: the config task did not answer, rebooting without the config downgrade");
    }
}

/// Largest image the registered target accepts, or [`OTA_MAX_IMAGE_BYTES`]
/// when none is registered, which only gates the pre-lock size check.
fn max_image_bytes() -> u64 {
    crate::ota::ota_info().map_or(OTA_MAX_IMAGE_BYTES, |info| info.max_image_bytes)
}

/// The dotted-quad IPv4 string, empty until DHCP assigns an address.
pub(crate) fn build_ip_string(net: ubos_net::net::Handle) -> String<16> {
    let mut result = String::new();
    if let Some(octets) = net
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

/// The colon-separated lowercase hex MAC string, empty until the board
/// publishes its MAC.
pub(crate) fn build_mac_string(net: ubos_net::net::Handle) -> String<17> {
    let mut result = String::new();
    if let Some(mac) = net.current().and_then(|state| state.mac) {
        let _ = core::write!(
            result,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );
    }
    result
}

/// The `HH:MM:SS UTC` time string, empty until NTP synchronizes.
pub(crate) fn build_time_string(net: ubos_net::net::Handle) -> String<24> {
    let mut result = String::new();
    if let Some(unix_secs) = net.unix_secs() {
        let time = ubos_net::ntp::format_utc_time(unix_secs);
        let _ = result.push_str(time.as_str());
    }
    result
}

/// Write a 405 with the `Allow` header RFC 7231 requires on it.
async fn write_method_not_allowed<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    allowed: &'static str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let body = b"method not allowed";
    let content_length = ubos_http::format_content_length(body.len());

    let mut headers: heapless::Vec<(&str, &str), { ubos_http::MAX_RESPONSE_HEADERS }> =
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

    ubos_http::write_response(connection, Status::METHOD_NOT_ALLOWED, &headers, body).await
}

/// Serialize `value` to `scratch`, then write a 200 JSON response, or a
/// 500 when serialization fails, which the plain serde derivations make
/// a firmware bug.
async fn serialize_and_respond<T, const N: usize, V: serde::Serialize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    value: &V,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if let Ok(length) = serde_json_core::to_slice(value, scratch) {
        write_json_response(connection, Status::OK, &scratch[..length]).await
    } else {
        // The body stays JSON so a machine client parses the failure with
        // the same decoder.
        write_message(
            connection,
            Status::INTERNAL_SERVER_ERROR.with_message("serialization error"),
        )
        .await
    }
}

/// Handle `GET /api/system/info`.
async fn handle_info<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    service: &RestApiService,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let RestApiService {
        board_name,
        build,
        mining_info,
        ..
    } = *service;
    let uptime_seconds = Instant::now().as_secs();
    let chain_stats = service.mining.chains();
    let hashrate_hps = chain_stats.machine_hashrate();
    let fan = service.cooling.current();
    let fan_speed = fan.map_or_else(FanRpm::default, |f| f.rpm);
    let fan_pwm = fan.map_or_else(FanPwm::default, |f| f.pwm);
    let pools_bitmap = service.stratum.pool_alive_bitmap();
    let shares_accepted = service.stratum.shares_accepted();
    let shares_rejected = service.stratum.shares_rejected();
    let shares_total = chain_stats.machine_share_count();
    // The `Config` snapshot is dropped before the await, so only the
    // hostname rides the response future.
    let configured_hostname = service.config.current().await.network.hostname;
    let hostname = if configured_hostname.is_empty() {
        board_name
    } else {
        configured_hostname.as_str()
    };

    let ota_info = crate::ota::ota_info();

    let mining_state = service
        .mining
        .state()
        .map(crate::schema::state::ApiMiningState::from);
    let (paused, pause_reason) = crate::rest::pause_state(
        service.thermal.is_dangerous(),
        service.mining.temperature_unreadable(),
        service.manager.pause_reasons(),
    );

    let hw_info = HwInfo {
        brand_name: crate::hw_info::brand_name(),
        product_name: crate::hw_info::product_name(),
        nominal_hashrate: crate::hw_info::nominal_hashrate(),
        bootloader_version: crate::hw_info::bootloader_version(),
        fan_controller_version: crate::hw_info::fan_controller_version(),
        psu_model: service.psu.model(),
        hashboard_ids: crate::hw_info::hashboard_ids(),
        chains_present: chain_stats.present_chain_count(),
    };

    let info = SystemInfo {
        api_variant: API_VARIANT,
        board_name,
        build,
        hostname,
        hashrate: hashrate_hps,
        uptime_seconds,
        fan_speed,
        fan_pwm,
        pools_bitmap,
        ip: build_ip_string(service.net),
        time: build_time_string(service.net),
        asic_count: mining_info.asic_count,
        asic_model: mining_info.asic_model,
        default_frequency: mining_info.default_frequency,
        default_voltage: mining_info.default_voltage,
        shares_accepted,
        shares_rejected,
        shares_total,
        best_nonce_diff: 0,
        best_session_nonce_diff: 0,
        ota: ota_info,
        mining_state,
        paused,
        pause_reason,
        hw_info,
    };

    serialize_and_respond(connection, scratch, &info).await
}

/// Handle `GET /api/system/asic`.
async fn handle_asic<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    mining_info: &'static ubos_config::MiningInfo,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(connection, scratch, mining_info).await
}

/// Handle `GET /api/system/thermal`: the accepted ranges, so the webui
/// pulls its input bounds from the source the firmware validates against.
async fn handle_thermal<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(connection, scratch, &ubos_config::THERMAL_INFO).await
}

/// Handle `GET /api/system/temperature`.
///
/// Returns the temperatures the thermal loop filters over its window,
/// the ambient reading and the kind the cooling PID follows.
async fn handle_temperature<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    thermal: ThermalHandle,
    cooling: ubos_cooling::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let temps = thermal.filtered();
    let celsius = |source: Source, statistic: Statistic| {
        temps
            .and_then(|temps| temps.temp_stats(source))
            .map(|stats| stats.temp(statistic).celsius_i32())
    };
    let body = SystemTemperature {
        chip_max: celsius(Source::Chip, Statistic::Max),
        chip_mean: celsius(Source::Chip, Statistic::Mean),
        hashboard_max: celsius(Source::Hashboard, Statistic::Max),
        hashboard_mean: celsius(Source::Hashboard, Statistic::Mean),
        ambient: thermal.ambient().map(Temperature::celsius_i32),
        pid_source: cooling.current().and_then(|fan| fan.pid_source),
    };
    serialize_and_respond(connection, scratch, &body).await
}

/// Handle `GET /api/system/cooling`; see [`handle_thermal`].
async fn handle_cooling<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    serialize_and_respond(connection, scratch, &ubos_config::COOLING_INFO).await
}

/// Handle `GET /api/system/statistics`: the fixed label set with no rows.
async fn handle_statistics<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    net: ubos_net::net::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let current_timestamp = net
        .unix_secs()
        .map_or(0, |unix_secs| unix_secs.saturating_mul(1_000));

    let statistics = SystemStatistics {
        current_timestamp,
        labels: LABELS,
        statistics: &[],
    };

    serialize_and_respond(connection, scratch, &statistics).await
}

/// Handle `GET /api/system/scoreboard`: an empty array.
async fn handle_scoreboard<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let entries: &[SystemScoreboardEntry] = &[];
    if let Ok(length) = serde_json_core::to_slice(entries, scratch) {
        write_json_response(connection, Status::OK, &scratch[..length]).await
    } else {
        write_message(
            connection,
            Status::INTERNAL_SERVER_ERROR.with_message("serialization error"),
        )
        .await
    }
}

/// Handle a not-implemented endpoint with a 501 JSON response.
async fn handle_not_implemented<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    message: &'static str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    write_message(connection, Status::NOT_IMPLEMENTED.with_message(message)).await
}

/// Write a 200 with `{"message":"ack"}`.
async fn write_ack_response<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    write_message(connection, Status::OK.with_message("ack")).await
}

/// Handle `GET /api/system/network`.
async fn handle_network_get<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    scratch: &mut [u8],
    stack: Stack<'static>,
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let seconds_remaining = config.pending_seconds_remaining();
    // While a change is pending the live address already reflects the
    // preview, so the previewed mode is reported with it.
    let mode = match config.pending_mode_is_static() {
        Some(true) => "static",
        Some(false) => "dhcp",
        None => match config.current().await.network.mode {
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
            // Same capacity as `dns_servers`, so the push cannot fail.
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
    serialize_and_respond(connection, scratch, &status).await
}

/// Request body for `POST /api/system/network`.
///
/// Carries the network config fields beside `confirm_after_switch`,
/// which the caller always sends.
#[derive(serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub(crate) struct NetworkPostRequest {
    /// Network mode to apply (DHCP or static).
    mode: ubos_config::NetworkMode,
    /// Desired hostname for the device on the network.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = ubos_config::network::HOSTNAME_MAX)))]
    hostname: heapless::String<{ ubos_config::network::HOSTNAME_MAX }>,
    /// When `true`, arm the 120 s confirm-or-revert window. When `false`,
    /// apply the change live and persist it immediately with no revert.
    confirm_after_switch: bool,
}

/// Handle `POST /api/system/network`: validate the body and forward it as
/// [`ubos_config::Message::ApplyNetwork`].
async fn handle_network_post<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    body: &[u8],
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let Ok((request, _)) = serde_json_core::de::from_slice::<NetworkPostRequest>(body) else {
        return write_message(connection, Status::BAD_REQUEST.with_message("invalid JSON")).await;
    };
    let network = ubos_config::NetworkConfig {
        mode: request.mode,
        hostname: request.hostname,
    };
    if let Err(message) = network.validate() {
        return write_message(connection, Status::BAD_REQUEST.with_message(message)).await;
    }
    // A static change without the confirm window persists an address a
    // reboot cannot recover from. DHCP self-heals, so it can opt out.
    if matches!(network.mode, ubos_config::NetworkMode::Static { .. })
        && !request.confirm_after_switch
    {
        return write_message(
            connection,
            Status::BAD_REQUEST.with_message("static address changes require the confirm window"),
        )
        .await;
    }
    if !config.try_send(ubos_config::Message::ApplyNetwork {
        network,
        confirm: request.confirm_after_switch,
    }) {
        return write_busy(connection).await;
    }
    write_json_response(connection, Status::OK, b"{\"ok\":true}").await
}

/// Write a 503 when the bounded apply channel is full, so the operator
/// retries instead of seeing a success that never took effect.
async fn write_busy<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    write_message(
        connection,
        Status::SERVICE_UNAVAILABLE.with_message("busy, retry"),
    )
    .await
}

/// Handle `POST /api/system/network/confirm`.
async fn handle_network_confirm<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if !config.try_send(ubos_config::Message::ConfirmNetwork) {
        return write_busy(connection).await;
    }
    write_ack_response(connection).await
}

/// Handle `POST /api/system/network/revert`.
async fn handle_network_revert<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if !config.try_send(ubos_config::Message::RevertNetwork) {
        return write_busy(connection).await;
    }
    write_ack_response(connection).await
}

/// Handle a POST action endpoint that only logs and acks. Pause, resume
/// and the overheat clear carry a `Manager` side effect and are
/// dispatched separately.
async fn handle_action<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    action: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    info!("api action: {}", action);
    write_ack_response(connection).await
}

/// Handle `POST /api/system/resume`. `Start` lifts only the operator's
/// own pause, so a resume while the dangerous temperature flag is set
/// answers 409 rather than acking a halt that stands; see "The overheat
/// halt" in `docs/MINING.md`.
pub(crate) async fn handle_resume<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    manager: ManagerHandle,
    thermal: ThermalHandle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    if thermal.is_dangerous() {
        return write_message(
            connection,
            Status::CONFLICT.with_message("an overheat halt stands; clear it to start"),
        )
        .await;
    }
    crate::events::user_requested_start();
    manager.send(ManagerMessage::Start).await;
    handle_action(connection, "resume").await
}

/// Handle `POST /api/system/thermal/clear`, which clears the halt and
/// starts mining. It logs the start as `handle_resume` does, because on
/// either route the operator asked for mining to start.
pub(crate) async fn handle_thermal_clear<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    manager: ManagerHandle,
    thermal: ThermalHandle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    crate::events::user_cleared_thermal_halt();
    crate::events::user_requested_start();
    thermal.acknowledge();
    manager.send(ManagerMessage::Start).await;
    handle_action(connection, "thermal/clear").await
}

/// Handle `POST /api/system/factory_reset`. The config task wipes the
/// config and only then requests the reboot, so the erase completes
/// before the reset. Sessions are cleared up front so the cookie cannot
/// authenticate in the window before the reset fires.
async fn handle_factory_reset<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ubos_auth::session::clear_all_sessions();
    config.send(ubos_config::Message::FactoryReset).await;
    write_ack_response(connection).await
}

/// Handle `PATCH /api/system`: a stub that acks. The body is not logged,
/// since a client can send credentials in it.
async fn handle_patch_system<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    body_len: usize,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    info!("api patch /api/system ({} bytes)", body_len);
    write_ack_response(connection).await
}

/// A failed [`read_chunk`], carrying the bytes read before the failure.
enum ChunkReadError {
    Eof { read_so_far: usize },
    Io { read_so_far: usize },
}

/// Fills `buffer[..want]` from `connection`, looping over short reads.
/// Returns the failure instead of acting on it, so each caller keeps its
/// own wording. Free because `Connection` is `edge-http`'s type.
async fn read_chunk<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    buffer: &mut [u8],
    want: usize,
) -> Result<(), ChunkReadError>
where
    T: Read + Write + TcpSplit,
{
    let mut chunk_read = 0usize;
    while chunk_read < want {
        match embedded_io_async::Read::read(connection, &mut buffer[chunk_read..want]).await {
            Ok(0) => {
                return Err(ChunkReadError::Eof {
                    read_so_far: chunk_read,
                });
            }
            Ok(read) => chunk_read += read,
            Err(_) => {
                return Err(ChunkReadError::Io {
                    read_so_far: chunk_read,
                });
            }
        }
    }
    Ok(())
}

/// Ends an upload that failed partway with `error`. It records the error,
/// aborts the write of the target, resumes mining so the board keeps
/// hashing, and answers the client, which is what every failure after
/// `begin` does.
async fn fail_upload<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    ota: &mut dyn OtaTarget,
    manager: ManagerHandle,
    error: OtaError,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    ota_session().fail(ota, error);
    manager.send(ManagerMessage::OtaEnd).await;
    write_message(connection, error.into()).await
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
/// - 503 — mining failed to stop within the timeout.
/// - the [`ubos_http::StatusMessage`] of the [`OtaError`] when `begin`, a read of the
///   body, `write_chunk` or `finish` fails.
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_ota<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    content_length: Option<u64>,
    manager: ManagerHandle,
    mining: ubos_mining::Handle,
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    // Rejected on headers before the mutex, so a bad request never holds
    // the upload lock.
    let Some(image_bytes) = content_length else {
        return write_message(
            connection,
            Status::LENGTH_REQUIRED.with_message("Content-Length required for OTA upload"),
        )
        .await;
    };

    if image_bytes > max_image_bytes() {
        return write_message(
            connection,
            Status::PAYLOAD_TOO_LARGE.with_message("image exceeds maximum slot size"),
        )
        .await;
    }

    if image_bytes < OTA_MIN_IMAGE_BYTES {
        // No session has started, so the phase stays Idle rather than
        // reporting a client mistake as a device error.
        return write_message(
            connection,
            Status::BAD_REQUEST.with_message("image too small"),
        )
        .await;
    }

    // `try_lock`, so a second upload is rejected rather than queued behind
    // a write that ends in a reboot.
    let Ok(mut guard) = crate::ota::target_mutex().try_lock() else {
        return write_message(
            connection,
            Status::CONFLICT.with_message("an update is already in progress"),
        )
        .await;
    };
    let Some(ota) = guard.as_deref_mut() else {
        return write_message(
            connection,
            Status::NOT_IMPLEMENTED.with_message("OTA not available on this board"),
        )
        .await;
    };

    #[allow(clippy::cast_possible_truncation)]
    let total_bytes = image_bytes as usize;

    // Mining stops before any flash access: an in-flight UART DMA transfer
    // on core 1 can race a flash write.
    if !quiesce_mining(manager, mining).await {
        warn!("OTA: mining did not stop within timeout; aborting");
        manager.send(ManagerMessage::OtaEnd).await;
        return write_message(
            connection,
            Status::SERVICE_UNAVAILABLE.with_message("mining did not stop in time; retry"),
        )
        .await;
    }

    match ota.begin() {
        Ok(()) => {
            ota_session().begin();
        }
        Err(OtaError::NoPartitionTable) => {
            manager.send(ManagerMessage::OtaEnd).await;
            return write_message(connection, OtaError::NoPartitionTable.into()).await;
        }
        Err(error) => {
            warn!("OTA begin failed: {:?}", error);
            ota_session().record_error(error);
            manager.send(ManagerMessage::OtaEnd).await;
            return write_message(connection, error.into()).await;
        }
    }

    let mut chunk_buffer = [0u8; OTA_CHUNK_SIZE];
    let mut bytes_written = 0usize;

    while bytes_written < total_bytes {
        let want = (total_bytes - bytes_written).min(OTA_CHUNK_SIZE);

        match read_chunk(connection, &mut chunk_buffer, want).await {
            Ok(()) => {}
            Err(ChunkReadError::Eof { read_so_far }) => {
                warn!(
                    "OTA: premature EOF at {}/{} bytes",
                    bytes_written + read_so_far,
                    total_bytes,
                );
                return fail_upload(connection, ota, manager, OtaError::ShortUpload).await;
            }
            Err(ChunkReadError::Io { .. }) => {
                warn!("OTA: read error after {} bytes", bytes_written);
                return fail_upload(connection, ota, manager, OtaError::Connection).await;
            }
        }

        if let Err(error) = ota.write_chunk(&chunk_buffer[..want]) {
            warn!("OTA write_chunk failed: {:?}", error);
            return fail_upload(connection, ota, manager, error).await;
        }
        bytes_written += want;
        #[allow(clippy::cast_possible_truncation)]
        ota_session().chunk_written(bytes_written as u32);
        // See `OTA_DRAIN_DELAY_MS` for why this is a delay and not a yield.
        Timer::after(Duration::from_millis(OTA_DRAIN_DELAY_MS)).await;
    }

    if let Err(error) = ota.finish() {
        warn!("OTA finish failed: {:?}", error);
        return fail_upload(connection, ota, manager, error).await;
    }

    downgrade_config(ota, config).await;
    info!(
        "OTA upload complete ({} bytes); rebooting into new slot",
        total_bytes
    );
    ota_session().rebooting();
    // `finish()` marked the swap, so the reboot happens whether or not the
    // ack reaches the client; skipping it would leave mining stopped and
    // the swap pending.
    let _ = write_json_response(connection, Status::OK, b"{\"ok\":true}").await;

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
/// - 400 — non-final chunk shorter than one sector.
/// - the [`ubos_http::StatusMessage`] of the [`OtaError`] when `begin`, a read of the
///   chunk, `write_chunk` or `finish` fails, or when the final chunk ends
///   an image smaller than [`OTA_MIN_IMAGE_BYTES`].
///
/// Success responses:
/// - Mid-stream: 200 `{"ok":true,"next":<offset+n>}`.
/// - Final chunk: 200 `{"ok":true}` then the board reboots.
///
/// `ota_offset` is the parsed value of `X-OTA-Offset`.
/// `ota_final` is `true` when `X-OTA-Final` equals `"1"`.
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_ota_chunk<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    content_length: Option<u64>,
    ota_offset: u64,
    ota_final: bool,
    manager: ManagerHandle,
    mining: ubos_mining::Handle,
    config: ubos_config::Handle,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    // Rejected on headers before the mutex, so a bad request never holds
    // the upload lock.
    let Some(chunk_bytes) = content_length else {
        return write_message(
            connection,
            Status::LENGTH_REQUIRED.with_message("Content-Length required for OTA chunk"),
        )
        .await;
    };

    if chunk_bytes > OTA_CHUNK_SIZE as u64 {
        return write_message(
            connection,
            Status::PAYLOAD_TOO_LARGE.with_message("chunk exceeds maximum sector size"),
        )
        .await;
    }

    if chunk_bytes == 0 {
        return write_message(connection, Status::BAD_REQUEST.with_message("empty chunk")).await;
    }

    if ota_offset.saturating_add(chunk_bytes) > max_image_bytes() {
        return write_message(
            connection,
            Status::PAYLOAD_TOO_LARGE.with_message("image exceeds maximum slot size"),
        )
        .await;
    }

    // `try_lock`, so a second upload is rejected rather than queued behind
    // a write that ends in a reboot.
    let Ok(mut guard) = crate::ota::target_mutex().try_lock() else {
        return write_message(
            connection,
            Status::CONFLICT.with_message("an update is already in progress"),
        )
        .await;
    };
    let Some(ota) = guard.as_deref_mut() else {
        return write_message(
            connection,
            Status::NOT_IMPLEMENTED.with_message("OTA not available on this board"),
        )
        .await;
    };

    #[allow(clippy::cast_possible_truncation)]
    let chunk_size = chunk_bytes as usize;

    if ota_offset == 0 {
        // A prior session still active is aborted first, so its core park,
        // breadcrumb and write offset are reset before the slot re-arms.
        if ota.bytes_written().is_some() {
            warn!("OTA chunk: offset 0 with active session; aborting prior session");
            ota.abort();
        }
        if !quiesce_mining(manager, mining).await {
            warn!("OTA chunk: mining did not stop within timeout; aborting");
            manager.send(ManagerMessage::OtaEnd).await;
            return write_message(
                connection,
                Status::SERVICE_UNAVAILABLE.with_message("mining did not stop in time; retry"),
            )
            .await;
        }

        match ota.begin() {
            Ok(()) => {
                ota_session().begin();
            }
            Err(OtaError::NoPartitionTable) => {
                manager.send(ManagerMessage::OtaEnd).await;
                return write_message(connection, OtaError::NoPartitionTable.into()).await;
            }
            Err(error) => {
                warn!("OTA chunk begin failed: {:?}", error);
                ota_session().record_error(error);
                manager.send(ManagerMessage::OtaEnd).await;
                return write_message(connection, error.into()).await;
            }
        }
    } else {
        // A mismatch is an out-of-order or replayed request.
        let expected = ota.bytes_written().unwrap_or(0);
        #[allow(clippy::cast_possible_truncation)]
        let ota_offset_usize = ota_offset as usize;
        if ota_offset_usize != expected {
            warn!(
                "OTA chunk: offset mismatch: got {} expected {}",
                ota_offset_usize, expected,
            );
            return fail_upload(connection, ota, manager, OtaError::OffsetMismatch).await;
        }
    }

    // Every chunk but the last must cover exactly one erase sector.
    // write_chunk rebuilds a fresh updater per call and assumes one chunk
    // maps to one sector; a short non-final chunk would leave write_offset
    // off a sector boundary and let the next chunk re-erase a written sector.
    // The session is armed by this point, so the rejection ends it.
    if !ota_final && chunk_size != OTA_CHUNK_SIZE {
        warn!(
            "OTA chunk: non-final chunk is {} bytes, not a full sector",
            chunk_size,
        );
        return fail_upload(connection, ota, manager, OtaError::ShortChunk).await;
    }

    let mut chunk_buffer = [0u8; OTA_CHUNK_SIZE];

    match read_chunk(connection, &mut chunk_buffer, chunk_size).await {
        Ok(()) => {}
        Err(ChunkReadError::Eof { read_so_far }) => {
            warn!(
                "OTA chunk: premature EOF at {}/{} bytes",
                read_so_far, chunk_size,
            );
            return fail_upload(connection, ota, manager, OtaError::ShortUpload).await;
        }
        Err(ChunkReadError::Io { read_so_far }) => {
            warn!("OTA chunk: read error after {} bytes", read_so_far);
            return fail_upload(connection, ota, manager, OtaError::Connection).await;
        }
    }

    if let Err(error) = ota.write_chunk(&chunk_buffer[..chunk_size]) {
        warn!("OTA chunk write_chunk failed: {:?}", error);
        return fail_upload(connection, ota, manager, error).await;
    }

    // Both terms are bounded by the target's max image size.
    #[allow(clippy::cast_possible_truncation)]
    ota_session().chunk_written((ota_offset as u32).saturating_add(chunk_size as u32));

    if ota_final {
        let total_image_bytes = ota_offset.saturating_add(chunk_size as u64);
        if total_image_bytes < OTA_MIN_IMAGE_BYTES {
            return fail_upload(connection, ota, manager, OtaError::ShortUpload).await;
        }

        if let Err(error) = ota.finish() {
            warn!("OTA chunk finish failed: {:?}", error);
            return fail_upload(connection, ota, manager, error).await;
        }

        downgrade_config(ota, config).await;
        #[allow(clippy::cast_possible_truncation)]
        let total_bytes = ota_offset as usize + chunk_size;
        info!(
            "OTA chunk upload complete ({} bytes); rebooting into new slot",
            total_bytes,
        );
        ota_session().rebooting();
        // `finish()` marked the swap, so the reboot happens whether or not
        // the ack reaches the client; see `handle_ota`.
        let _ = write_json_response(connection, Status::OK, b"{\"ok\":true}").await;
        ota.reboot_into_new()
    }
    #[allow(clippy::cast_possible_truncation)]
    let next_offset = ota_offset as usize + chunk_size;
    let mut ack_buf = [0u8; 40];
    let mut writer = ubos_http::BufferWriter::new(&mut ack_buf);
    // Plain integer args, so `write!` cannot fail.
    let _ = core::write!(writer, "{{\"ok\":true,\"next\":{next_offset}}}");
    let ack_len = writer.len();
    write_json_response(connection, Status::OK, &ack_buf[..ack_len]).await
}

/// Begin a JSON response with no `Content-Length`, which edge-http sends
/// as chunked transfer encoding, so the caller writes the body in pieces.
async fn begin_streamed_json<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let mut headers: heapless::Vec<(&str, &str), { ubos_http::MAX_RESPONSE_HEADERS }> =
        heapless::Vec::new();
    headers
        .push(("Content-Type", "application/json"))
        .expect("BUG: response header vec sized for max headers");
    headers
        .push(("Cache-Control", "no-store"))
        .expect("BUG: response header vec sized for max headers");
    start_response(connection, Status::OK, &headers).await
}

/// The `?since=<u32>` query value, or zero when absent or unparseable. An
/// overflow clamps to `u32::MAX`, so an oversized cursor returns no
/// records rather than the whole ring.
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

/// Handle `GET /api/system/events`: the ring as `{"events":[...]}`,
/// oldest first, after `?since=<seq>`. The ring is read in batches by a
/// `seq` cursor, so no buffer scales with its capacity, and the read is
/// bounded to the records present when it began. A ring that wrapped past
/// `since` resumes at the oldest live record, and the `seq` jump is how
/// the browser sees the gap.
async fn handle_events<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    path: &str,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    let latest_seq = ubos_events::latest_seq();
    let mut cursor = parse_since(path);

    begin_streamed_json(connection).await?;
    embedded_io_async::Write::write_all(connection, EVENTS_OPEN).await?;

    let mut first_emitted = true;
    let mut batch = [ubos_events::EventRecord::EMPTY; EVENT_BATCH];
    while cursor < latest_seq {
        let count = ubos_events::snapshot_after(&mut batch, cursor);
        if count == 0 {
            break;
        }
        for record in &batch[..count] {
            cursor = record.seq;
            let mut buffer = [0u8; EVENT_JSON_MAX];
            let Ok(written) =
                serde_json_core::to_slice(&EventRecordJson::from(record), &mut buffer)
            else {
                warn!(
                    "events: record seq {} exceeds {}B buffer; skipped",
                    record.seq, EVENT_JSON_MAX
                );
                continue;
            };
            if !first_emitted {
                embedded_io_async::Write::write_all(connection, b",").await?;
            }
            embedded_io_async::Write::write_all(connection, &buffer[..written]).await?;
            first_emitted = false;
        }
    }

    embedded_io_async::Write::write_all(connection, EVENTS_CLOSE).await?;
    Ok(())
}

/// Handle `GET /api/system/events/catalog`: `[]` until the firmware
/// installs the catalogs.
async fn handle_events_catalog<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
) -> Result<(), HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    begin_streamed_json(connection).await?;
    embedded_io_async::Write::write_all(connection, b"[").await?;

    let mut first_def = true;
    for catalog in ubos_events::catalogs() {
        for def in *catalog {
            let mut buffer = [0u8; DEF_JSON_MAX];
            let Ok(written) = serde_json_core::to_slice(&EventDefJson::from(def), &mut buffer)
            else {
                warn!(
                    "events: catalog def code {} exceeds {}B buffer; skipped",
                    def.code, DEF_JSON_MAX
                );
                continue;
            };
            if !first_def {
                embedded_io_async::Write::write_all(connection, b",").await?;
            }
            embedded_io_async::Write::write_all(connection, &buffer[..written]).await?;
            first_def = false;
        }
    }

    embedded_io_async::Write::write_all(connection, b"]").await?;
    Ok(())
}

/// The framing the events handler streams around its records: the
/// opening of [`crate::schema::events::EventsResponse`] and its close.
/// A test holds them to that struct's serialization.
pub(crate) const EVENTS_OPEN: &[u8] = b"{\"events\":[";
pub(crate) const EVENTS_CLOSE: &[u8] = b"]}";

/// Records copied from the ring per batch, so the events endpoint costs
/// no RAM that scales with the ring capacity.
const EVENT_BATCH: usize = 16;

/// Longest serialised event record, with its leading comma, plus headroom.
const EVENT_JSON_MAX: usize = 128;

/// Longest serialised event definition: a full-width code, a severity, a
/// format string of about 169 bytes and six quoted arg widths.
const DEF_JSON_MAX: usize = 256;

/// A known route on the `/api/system/*` surface.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum Route {
    /// `GET /api/system/info`
    SystemInfo,
    /// `GET /api/system/asic`
    SystemAsic,
    /// `GET /api/system/thermal`
    SystemThermal,
    /// `GET /api/system/temperature`
    SystemTemperature,
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
    /// `POST /api/system/thermal/clear`
    ThermalClearAck,
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

// The host tests read the surface off the enum: the OpenAPI route
// table takes each operation's method and path from `endpoint`, and
// `ALL` lets a test walk every variant. The firmware never asks.
#[cfg(test)]
impl Route {
    /// Every variant once, in the order the enum declares them.
    pub(crate) const ALL: [Self; 25] = [
        Self::SystemInfo,
        Self::SystemAsic,
        Self::SystemThermal,
        Self::SystemTemperature,
        Self::SystemCooling,
        Self::SystemStatistics,
        Self::SystemScoreboard,
        Self::SystemEvents,
        Self::SystemEventsCatalog,
        Self::SystemWifiScan,
        Self::SystemOta,
        Self::SystemOtaChunk,
        Self::SystemOtaWww,
        Self::PauseAck,
        Self::ResumeAck,
        Self::ThermalClearAck,
        Self::RestartAck,
        Self::IdentifyAck,
        Self::BlockFoundDismissAck,
        Self::PatchSystem,
        Self::FactoryReset,
        Self::NetworkGet,
        Self::NetworkPost,
        Self::NetworkConfirm,
        Self::NetworkRevert,
    ];

    /// The method and path `route` answers this variant for.
    pub(crate) const fn endpoint(self) -> (&'static str, &'static str) {
        match self {
            Self::SystemInfo => ("GET", "/api/system/info"),
            Self::SystemAsic => ("GET", "/api/system/asic"),
            Self::SystemThermal => ("GET", "/api/system/thermal"),
            Self::SystemTemperature => ("GET", "/api/system/temperature"),
            Self::SystemCooling => ("GET", "/api/system/cooling"),
            Self::SystemStatistics => ("GET", "/api/system/statistics"),
            Self::SystemScoreboard => ("GET", "/api/system/scoreboard"),
            Self::SystemEvents => ("GET", "/api/system/events"),
            Self::SystemEventsCatalog => ("GET", "/api/system/events/catalog"),
            Self::SystemWifiScan => ("GET", "/api/system/wifi/scan"),
            Self::SystemOta => ("POST", "/api/system/OTA"),
            Self::SystemOtaChunk => ("POST", "/api/system/OTA/chunk"),
            Self::SystemOtaWww => ("POST", "/api/system/OTAWWW"),
            Self::PauseAck => ("POST", "/api/system/pause"),
            Self::ResumeAck => ("POST", "/api/system/resume"),
            Self::ThermalClearAck => ("POST", "/api/system/thermal/clear"),
            Self::RestartAck => ("POST", "/api/system/restart"),
            Self::IdentifyAck => ("POST", "/api/system/identify"),
            Self::BlockFoundDismissAck => ("POST", "/api/system/blockFound/dismiss"),
            Self::PatchSystem => ("PATCH", "/api/system"),
            Self::FactoryReset => ("POST", "/api/system/factory_reset"),
            Self::NetworkGet => ("GET", "/api/system/network"),
            Self::NetworkPost => ("POST", "/api/system/network"),
            Self::NetworkConfirm => ("POST", "/api/system/network/confirm"),
            Self::NetworkRevert => ("POST", "/api/system/network/revert"),
        }
    }
}

/// Outcome of `route()`, kept apart from the response so the table is
/// testable without a `Connection`.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum DispatchOutcome {
    Handled(Route),
    /// Known path, wrong method; carries the `Allow` value.
    MethodNotAllowed {
        allowed: &'static str,
    },
    NotFound,
}

/// Classify a method and path against the `/api/system/*` surface. Pure,
/// so host tests call it.
///
/// Every `GET` arm must stay free of side effects: the session cookie is
/// `SameSite=Lax`, so a cross-site link carries it on a top-level `GET`,
/// and a `GET` that acted would act for the site that linked. Nothing
/// checks this.
///
/// It is one arm per route, so its length grows with the table and not
/// with any logic.
#[allow(clippy::too_many_lines)]
pub(crate) fn route(method: &str, path: &str) -> DispatchOutcome {
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
        "/api/system/temperature" => match method {
            "GET" => DispatchOutcome::Handled(Route::SystemTemperature),
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
        "/api/system/thermal/clear" => match method {
            "POST" => DispatchOutcome::Handled(Route::ThermalClearAck),
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

/// Dispatch a `/api/system/*` request. `false` means the path is not on
/// this surface and the caller falls through. Crate-private because the
/// auth and CSRF gates live in `RestApiService::handle`.
#[allow(clippy::too_many_lines)]
pub(crate) async fn dispatch<T, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    service: &RestApiService,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<bool, HttpError<T::Error>>
where
    T: Read + Write + TcpSplit,
{
    // One scratch per dispatch: the handlers are mutually exclusive.
    let mut scratch = [0u8; SCRATCH_SIZE];
    let scratch = &mut scratch[..];

    match route(method, path) {
        DispatchOutcome::NotFound => return Ok(false),
        DispatchOutcome::MethodNotAllowed { allowed } => {
            write_method_not_allowed(connection, allowed).await?;
        }
        DispatchOutcome::Handled(Route::SystemInfo) => {
            handle_info(connection, scratch, service).await?;
        }
        DispatchOutcome::Handled(Route::SystemAsic) => {
            handle_asic(connection, scratch, service.mining_info).await?;
        }
        DispatchOutcome::Handled(Route::SystemThermal) => {
            handle_thermal(connection, scratch).await?;
        }
        DispatchOutcome::Handled(Route::SystemTemperature) => {
            handle_temperature(connection, scratch, service.thermal, service.cooling).await?;
        }
        DispatchOutcome::Handled(Route::SystemCooling) => {
            handle_cooling(connection, scratch).await?;
        }
        DispatchOutcome::Handled(Route::SystemStatistics) => {
            handle_statistics(connection, scratch, service.net).await?;
        }
        DispatchOutcome::Handled(Route::SystemScoreboard) => {
            handle_scoreboard(connection, scratch).await?;
        }
        DispatchOutcome::Handled(Route::SystemEvents) => {
            handle_events(connection, path).await?;
        }
        DispatchOutcome::Handled(Route::SystemEventsCatalog) => {
            handle_events_catalog(connection).await?;
        }
        DispatchOutcome::Handled(Route::SystemWifiScan | Route::SystemOtaWww) => {
            handle_not_implemented(connection, "unsupported on this platform").await?;
        }
        DispatchOutcome::Handled(Route::SystemOta) => {
            // Intercepted in rest.rs before the body is pre-read; this arm
            // only answers if that interception is ever missed.
            handle_not_implemented(connection, "OTA upload must use the streaming path").await?;
        }
        DispatchOutcome::Handled(Route::SystemOtaChunk) => {
            // As for `SystemOta` above.
            handle_not_implemented(connection, "OTA chunk upload must use the streaming path")
                .await?;
        }
        DispatchOutcome::Handled(Route::PauseAck) => {
            crate::events::user_paused();
            service.manager.send(ManagerMessage::Stop).await;
            handle_action(connection, "pause").await?;
        }
        DispatchOutcome::Handled(Route::ResumeAck) => {
            handle_resume(connection, service.manager, service.thermal).await?;
        }
        DispatchOutcome::Handled(Route::ThermalClearAck) => {
            handle_thermal_clear(connection, service.manager, service.thermal).await?;
        }
        DispatchOutcome::Handled(Route::RestartAck) => {
            crate::events::user_restart();
            handle_action(connection, "restart").await?;
            ubos_core::reboot::request_reboot();
        }
        DispatchOutcome::Handled(Route::IdentifyAck) => {
            handle_action(connection, "identify").await?;
        }
        DispatchOutcome::Handled(Route::BlockFoundDismissAck) => {
            handle_action(connection, "blockFound/dismiss").await?;
        }
        DispatchOutcome::Handled(Route::PatchSystem) => {
            handle_patch_system(connection, body.len()).await?;
        }
        DispatchOutcome::Handled(Route::FactoryReset) => {
            handle_factory_reset(connection, service.config).await?;
        }
        DispatchOutcome::Handled(Route::NetworkGet) => {
            handle_network_get(connection, scratch, service.stack, service.config).await?;
        }
        DispatchOutcome::Handled(Route::NetworkPost) => {
            handle_network_post(connection, body, service.config).await?;
        }
        DispatchOutcome::Handled(Route::NetworkConfirm) => {
            handle_network_confirm(connection, service.config).await?;
        }
        DispatchOutcome::Handled(Route::NetworkRevert) => {
            handle_network_revert(connection, service.config).await?;
        }
    }

    Ok(true)
}
