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

//! REST API handler.
//!
//! Sibling of `dashboard` and serves only the `/api/*` surface behind the
//! router the two share. Same auth profile as the dashboard: Basic for
//! machine clients, session cookie for the SPA, CSRF for
//! session-authenticated mutating requests.

use core::fmt::Write as _;

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use ubos_logging::{info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_thermal::Handle as ThermalHandle;

use ubos_http::{RequestParts, Service, read_body, write_json_response, write_status_only};

#[cfg(test)]
#[path = "tests/rest.rs"]
mod tests;

/// Body buffer size for JSON responses built by `/api/info`.
///
/// The largest `/api/info` JSON body with all fields populated is
/// roughly 600 bytes in practice (name≤42, the longest current
/// `ControlBoardDescriptor::name` + numeric fields + 16-byte IPv4 +
/// 17-byte MAC string + 16-byte time string + OTA static fields +
/// OTA diagnostic fields including the `resetWritePhase` string +
/// `running` bool + `paused` bool + `pause_reason` string up to
/// `"dangerousTemperature"` + a `chains` array with one entry per
/// chain the board actually owns, up to `ubos_metrics::chain::MAX_CHAINS` +
/// JSON framing). The `api_info_body_fits_in_buffer` test builds the
/// absolute worst case — every numeric field at its maximum decimal
/// width and a `chains` array at `ubos_metrics::chain::MAX_CHAINS` entries,
/// the most any board can report — and measures 1187 bytes. 512 bytes
/// overflowed once the OTA diagnostic fields were added, silently
/// truncating the tail; 1280 bytes covers the synthetic worst case,
/// which grew past the earlier 896-byte figure once the per-chain
/// `chains` array was added, and leaves room for future fields while
/// staying on the stack.
const API_INFO_BODY_BUFFER_SIZE: usize = 1280;

/// Body buffer for the POST /api/config acknowledgement.
const CONFIG_ACK_BODY: &[u8] = b"{\"ok\":true}";

/// Body buffer size for `GET /api/config` responses.
///
/// The widest [`ubos_config::ConfigUpdate`] snapshot the buffer has to
/// hold — three pool slots carrying 128-byte host and user strings and
/// an SV2 pool key, a static `network` mode with three resolvers and a
/// 32-byte hostname, a 32-byte `api` username and password, and every
/// numeric field at its longest decimal form — serializes to 2097
/// bytes. The SV2 key is what makes a pool slot widest: a `[u8; 32]`
/// serializes as a JSON number array costing up to four bytes an
/// element, which outruns the 128-byte SV1 password string. That total
/// is an upper bound rather than a body this handler can produce:
/// [`ubos_config::ConfigUpdate::from_config`] blanks `api.password`, so
/// every served snapshot is shorter by that field. It blanks nothing
/// else — the pool entries, SV1 passwords included, are served as
/// stored. The figure is measured rather than estimated:
/// `config_response_body_fits_in_buffer` in `tests/rest.rs` builds the
/// snapshot and pins its length exactly, so the figure quoted here
/// cannot drift, and it fails outright if a field addition pushes the
/// body past this buffer. 3072 bytes covers it with margin while
/// keeping the stack budget predictable. A body that outgrows the
/// buffer is caught rather than truncated: `to_slice` returns `Err` and
/// the handler answers 500.
///
/// Per-connection stack cost is `CONFIG_RESPONSE_BUFFER_SIZE` bytes twice
/// over, since [`BODY_READ_BUFFER_SIZE`] tracks this constant; both
/// multiply by `ControlBoardDescriptor::http_pool` for concurrent
/// connections. If you increase this constant, audit task-stack sizing for
/// all ubos-api consumers.
const CONFIG_RESPONSE_BUFFER_SIZE: usize = 3072;

/// Buffer for reading POST/PATCH request bodies.
///
/// Tracks [`CONFIG_RESPONSE_BUFFER_SIZE`] so the widest body this API can
/// serve is a body it will also accept back: `POST /api/config` is the
/// natural round-trip of the `GET /api/config` snapshot, and
/// `Api::REQUEST_BUFFER_SIZE` — sized to the socket RX window, not to any
/// body — left the widest snapshot 49 bytes over. The window bounds a
/// single socket read rather than the body, which `read_body` accumulates
/// across reads, so the two never had to agree. Requests that exceed this
/// cap receive a 413 response.
const BODY_READ_BUFFER_SIZE: usize = CONFIG_RESPONSE_BUFFER_SIZE;

/// Validate that a board name string contains no JSON-unsafe characters.
///
/// Returns `true` when `name` contains no `"` (0x22) or `\` (0x5c)
/// bytes. Any such byte would break the unescaped JSON string literal
/// in the `/api/info` response body, so callers that inject board names
/// into JSON must enforce this invariant at compile time.
const fn validate_board_name(name: &str) -> bool {
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

/// REST API handler implementing `ubos_http::Service`.
///
/// Serves only `/api/*` paths. Auth profile mirrors `DashboardService`:
/// Basic or session cookie with CSRF enforcement on
/// session-authenticated mutating requests.
pub struct RestApiService {
    /// The control board's product name, e.g. `"W6100-EVB-Pico2"`.
    pub(crate) board_name: &'static str,
    pub(crate) mining_info: &'static ubos_config::MiningInfo,
    pub(crate) bounds: &'static ubos_config::MiningBounds,
    pub(crate) thermal: ThermalHandle,
    pub(crate) manager: ManagerHandle,
    pub(crate) psu: ubos_psu::Handle,
    pub(crate) stack: Stack<'static>,
}

impl RestApiService {
    /// Construct the REST API service value.
    ///
    /// Pass the returned value to a `StaticCell::init` so its address
    /// is fixed for the firmware lifetime, then hand the `&'static`
    /// reference to each worker task spawned with `http_worker_task!`.
    #[must_use]
    pub fn new(
        board_name: &'static str,
        mining_info: &'static ubos_config::MiningInfo,
        bounds: &'static ubos_config::MiningBounds,
        thermal: ThermalHandle,
        manager: ManagerHandle,
        psu: ubos_psu::Handle,
        stack: Stack<'static>,
    ) -> Self {
        Self {
            board_name,
            mining_info,
            bounds,
            thermal,
            manager,
            psu,
            stack,
        }
    }
}

/// Write the `/api/info` `chains` array: one entry per chain the
/// board actually owns, which is what `chain_stats.chains()` yields,
/// rather than one entry per slot in the fixed-size table `chain_stats`
/// is backed by. A single-chain board must not advertise phantom chains
/// 1 and 2.
fn write_chains(
    writer: &mut ubos_http::BufferWriter<'_>,
    chain_stats: ubos_metrics::chain::ChainStatsHandle,
) {
    let _ = writer.write_str(",\"chains\":[");
    for (chain, stats) in chain_stats.chains().enumerate() {
        if chain > 0 {
            let _ = writer.write_str(",");
        }
        let state = match stats.presence() {
            ubos_metrics::chain::ChainPresence::Unknown => "unknown",
            ubos_metrics::chain::ChainPresence::Present { .. } => "present",
            ubos_metrics::chain::ChainPresence::Absent => "absent",
            ubos_metrics::chain::ChainPresence::Faulted { .. } => "faulted",
        };
        let _ = core::write!(
            writer,
            "{{\"chain\":{chain},\
             \"state\":\"{state}\",\
             \"chips\":{},\
             \"frequency_mhz\":{},\
             \"hashrate\":{},\
             \"hottest_c\":",
            stats.chips(),
            stats.frequency().mhz(),
            stats.hashrate(),
        );
        match stats.hottest_mc() {
            Some(hottest_mc) => {
                let _ = core::write!(writer, "{}", hottest_mc / 1000);
            }
            None => {
                let _ = writer.write_str("null");
            }
        }
        let _ = writer.write_str("}");
    }
    let _ = writer.write_str("]");
}

impl Service for RestApiService {
    // The line count comes from the flat match dispatch table and the
    // duplicated-from-dashboard handler bodies. Splitting each route into
    // a helper would require threading the generic `Connection` parameter
    // through every call, adding noise without reducing complexity.
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
        // Count each request that reaches this handler so
        // `board_http_requests_total` reflects dashboard and API traffic.
        // The metrics port does not increment it, and a connection
        // dropped before its request is parsed is never counted.
        ubos_net::net::handle().record_request();

        let method = parts.method.as_str();
        let path = parts.path.as_str();
        let authorization = parts.authorization.as_deref();
        let cookie = parts.cookie.as_deref();
        let content_length = parts.content_length;

        // Every route under this prefix requires auth; there are no
        // anonymous endpoints on the API surface. The CSRF check runs
        // separately below, once the request body has been read, so an
        // oversized body does not burn the session's CSRF token.
        let identity_policy = ubos_auth::Policy {
            allow_anonymous: false,
        };
        let auth_ctx =
            match ubos_auth::gate_identity(peer_ip, authorization, cookie, &identity_policy) {
                Err(ubos_auth::Rejection::NotAllowed) => {
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
                Err(_) => {
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
                Ok(auth_ctx) => auth_ctx,
            };

        let is_mutating = matches!(method, "POST" | "PATCH" | "DELETE");

        // The OTA firmware upload is the one body too large to pre-read into
        // `body_buffer` (it is ~1 MiB against a 3 KiB buffer). It is streamed
        // straight to flash by `handle_ota`, which reads the connection itself
        // in sector-sized chunks, so its body is not pre-read here.
        let is_ota_upload = is_mutating && method == "POST" && path == "/api/system/OTA";

        // The chunk upload body is at most OTA_CHUNK_SIZE (4096 bytes), which
        // exceeds BODY_READ_BUFFER_SIZE (3072 bytes), so it is also not
        // pre-read; `handle_ota_chunk` reads the connection directly.
        let is_ota_chunk_upload =
            is_mutating && method == "POST" && path == "/api/system/OTA/chunk";

        // Read mutating request bodies before the CSRF check so an
        // oversized payload returns 413 without consuming the token.
        let mut body_buffer = [0u8; BODY_READ_BUFFER_SIZE];
        let body: &[u8] = if is_mutating && !is_ota_upload && !is_ota_chunk_upload {
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

        // OTA continuation chunks (offset > 0) are exempt from CSRF. Only the
        // session-starting offset-0 chunk consumes a token. A continuation cannot
        // start an upload — it is bound to the single in-progress session by the
        // target mutex and the handler's offset-continuity check (409 on mismatch)
        // — so requiring a single-use CSRF token per 4 KiB sector would force ~128
        // tokens per image for no security gain. An attacker still cannot START an
        // OTA without a token, and a forged continuation only corrupts an in-flight
        // upload (fails validation -> auto-rollback), never installs firmware.
        let is_ota_continuation = is_ota_chunk_upload
            && parts
                .ota_offset
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|offset| offset > 0);

        // Session-authenticated mutating requests require a valid CSRF
        // token. Basic-authenticated requests are exempt, since only a
        // session cookie can be forged cross-site. The check takes the
        // identity resolved above rather than resolving it again, so the
        // body read between the two cannot change who is being checked.
        if is_mutating
            && !is_ota_continuation
            && ubos_auth::check_csrf(&auth_ctx, parts.csrf.as_deref()).is_err()
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

        // The OTA upload streams its body from the connection rather than a
        // pre-read buffer, so it is dispatched here directly with the parsed
        // content length. Auth and CSRF have already been enforced above.
        if is_ota_upload {
            return crate::system::handle_ota(connection, content_length, self.manager).await;
        }

        // The OTA chunk upload also streams its body (one sector at a time).
        // Parse X-OTA-Offset and X-OTA-Final before dispatching. Auth and CSRF
        // have already been enforced above; Basic-auth requests are CSRF-exempt
        // so curl-driven uploads work without a session token. Session-auth
        // callers supply one CSRF token on the offset-0 chunk (which starts the
        // session); continuation chunks are CSRF-exempt because the single-session
        // mutex and the offset-continuity check together prevent any attacker from
        // injecting data into a live upload. The web UI therefore uploads with one
        // CSRF token on the first chunk and bare credentials on the rest.
        if is_ota_chunk_upload {
            // Parse the decimal offset from X-OTA-Offset; missing or
            // unparseable offset is treated as a bad request.
            let ota_offset: u64 = match parts
                .ota_offset
                .as_deref()
                .and_then(|value| value.parse().ok())
            {
                Some(offset) => offset,
                None => {
                    return write_status_only(
                        connection,
                        400,
                        "Bad Request",
                        "application/json",
                        &[],
                        b"{\"message\":\"X-OTA-Offset header missing or invalid\"}",
                    )
                    .await;
                }
            };
            let ota_final = parts.ota_final.as_deref() == Some("1");
            return crate::system::handle_ota_chunk(
                connection,
                content_length,
                ota_offset,
                ota_final,
                self.manager,
            )
            .await;
        }

        // Route dispatch: only /api/* paths are served here.
        if path == "/api/system" || path.starts_with("/api/system/") {
            let handled = crate::system::dispatch(
                connection,
                method,
                path,
                body,
                self.board_name,
                self.psu,
                self.mining_info,
                self.thermal,
                self.manager,
                self.stack,
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
            ("GET", "/api/info") => {
                let uptime_secs = Instant::now().as_secs();
                let chain_stats = ubos_metrics::chain::handle();
                let hashrate = chain_stats.machine_hashrate();
                let thermal_reading = self.thermal.current();
                let chain_voltage_mv = thermal_reading
                    .as_ref()
                    .and_then(|reading| reading.chain_voltage_mv)
                    .unwrap_or(0);
                #[allow(clippy::cast_possible_truncation)]
                let temperature = thermal_reading.map_or(0i32, |r| r.chip_max.celsius_f32() as i32);
                let fan = ubos_cooling::handle().current();
                let fan_rpm = fan.map_or(0, |f| f.rpm.rpm());
                let fan_pwm = fan.map_or(0, |f| f.pwm.percent());
                let pools = stratum_client::pool_alive_bitmap();
                let psu_reading = self.psu.current();
                let power_in_mw = psu_reading.map_or(0u32, |r| r.input_power.mw());
                let power_out_mw = psu_reading.map_or(0u32, |r| r.output_power.mw());
                let psu_voltage_set_mv = psu_reading.map_or(0u32, |r| r.output_voltage_set.mv());
                let psu_voltage_measured_mv =
                    psu_reading.map_or(0u32, |r| r.output_voltage_measured.mv());
                let psu_fault = psu_reading.is_some_and(|r| r.sensor_fault);
                // Seconds left in the network confirm window, or 0 when nothing is pending.
                // Serialized as `net_confirm_seconds` in the JSON body.
                let net_confirm_seconds = ubos_config::pending_seconds_remaining();
                let pause_reasons = self.manager.pause_reasons();
                let paused = !pause_reasons.is_empty();
                let running = ubos_mining::handle().state().is_some_and(|s| s.running);

                let mut body_buf = [0u8; API_INFO_BODY_BUFFER_SIZE];
                let mut writer = ubos_http::BufferWriter::new(&mut body_buf);

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
                     \"power_in_mw\":{power_in_mw},\
                     \"power_out_mw\":{power_out_mw},\
                     \"psu_voltage_set_mv\":{psu_voltage_set_mv},\
                     \"psu_voltage_measured_mv\":{psu_voltage_measured_mv},\
                     \"chain_voltage_mv\":{chain_voltage_mv},\
                     \"psu_fault\":{psu_fault},\
                     \"pools\":{pools},\
                     \"net_confirm_seconds\":{net_confirm_seconds},\
                     \"running\":{running},\
                     \"paused\":{paused},\
                     \"pause_reason\":",
                    self.board_name
                );
                match pause_reasons.dominant() {
                    Some(reason) => {
                        let _ = core::write!(writer, "\"{}\"", reason.as_api_str());
                    }
                    None => {
                        let _ = writer.write_str("null");
                    }
                }
                write_chains(&mut writer, chain_stats);

                let _ = writer.write_str(",\"ip\":\"");

                let net_state = ubos_net::net::handle().current();
                if let Some(octets) = net_state.and_then(|state| state.ipv4).map(|ip| ip.octets()) {
                    let _ = core::write!(
                        writer,
                        "{}.{}.{}.{}",
                        octets[0],
                        octets[1],
                        octets[2],
                        octets[3]
                    );
                }

                let _ = writer.write_str("\",\"mac\":\"");
                if let Some(mac) = net_state.and_then(|state| state.mac) {
                    let _ = core::write!(
                        writer,
                        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                        mac[0],
                        mac[1],
                        mac[2],
                        mac[3],
                        mac[4],
                        mac[5]
                    );
                }

                let _ = writer.write_str("\",\"time\":\"");
                let ntp_offset = ubos_net::net::handle().ntp_epoch_offset();
                if ntp_offset != 0 {
                    let unix_secs = ubos_net::wall_clock_secs();
                    let time = ubos_net::ntp::format_utc_time(unix_secs);
                    let _ = writer.write_str(time.as_str());
                }
                let _ = writer.write_str("\"");

                // OTA block: the dashboard polls /api/info and only renders
                // the firmware card when this field is present. Read from
                // the lock-free published snapshot, never the upload mutex.
                if let Some(ota) = crate::ota::ota_info() {
                    let _ = core::write!(
                        writer,
                        ",\"ota\":{{\"runningSlot\":\"{}\",\
                         \"runningVersion\":{},\
                         \"hasPreviousImage\":{},\
                         \"previousVersion\":",
                        ota.running_slot,
                        ota.running_version,
                        ota.has_previous_image,
                    );
                    match ota.previous_version {
                        Some(version) => {
                            let _ = core::write!(writer, "{version}");
                        }
                        None => {
                            let _ = writer.write_str("null");
                        }
                    }
                    let _ = core::write!(writer, ",\"maxImageBytes\":{}", ota.max_image_bytes);

                    // Live upload session state: readable lock-free even
                    // while an upload holds the target mutex.
                    let session = crate::ota::ota_session().snapshot();
                    let phase_str = match session.phase {
                        crate::ota::OtaPhase::Idle => "idle",
                        crate::ota::OtaPhase::Uploading => "uploading",
                        crate::ota::OtaPhase::Error => "error",
                        crate::ota::OtaPhase::Rebooting => "rebooting",
                    };
                    let _ = core::write!(
                        writer,
                        ",\"phase\":\"{phase_str}\",\
                         \"bytesWritten\":{},\
                         \"chunks\":{},\
                         \"lastError\":{}",
                        session.bytes_written,
                        session.chunks,
                        session.last_error,
                    );

                    // Reset diagnostics: captured at boot from the watchdog
                    // reason register and the OTA breadcrumb.
                    let (
                        reset_reason_str,
                        reset_ota_active,
                        reset_ota_offset,
                        reset_write_phase_str,
                    ) = match crate::ota::reset_diag() {
                        Some(diag) => {
                            let reason_str = match diag.reason {
                                crate::ota::ResetReason::Unknown => "unknown",
                                crate::ota::ResetReason::Clean => "clean",
                                crate::ota::ResetReason::WatchdogTimeout => "watchdogTimeout",
                                crate::ota::ResetReason::WatchdogForced => "watchdogForced",
                            };
                            let phase_str = match diag.write_phase {
                                crate::ota::OtaWritePhase::None => "none",
                                crate::ota::OtaWritePhase::InFlash => "inFlash",
                                crate::ota::OtaWritePhase::FlashDone => "flashDone",
                                crate::ota::OtaWritePhase::Panic => "panic",
                            };
                            (reason_str, diag.ota_was_active, diag.ota_offset, phase_str)
                        }
                        None => ("unknown", false, 0u32, "none"),
                    };
                    let _ = core::write!(
                        writer,
                        ",\"resetReason\":\"{reset_reason_str}\",\
                         \"resetOtaActive\":{reset_ota_active},\
                         \"resetOtaOffset\":{reset_ota_offset},\
                         \"resetWritePhase\":\"{reset_write_phase_str}\"",
                    );

                    let _ = writer.write_str("}");
                }

                let _ = writer.write_str("}");
                let json_len = writer.len();

                write_json_response(connection, 200, "OK", &body_buf[..json_len]).await
            }
            ("GET", "/api/config") => {
                let mut body_buf = [0u8; CONFIG_RESPONSE_BUFFER_SIZE];
                // Drop the snapshot and the response struct before the
                // await so neither lives on the rest_api future stack
                // across write_json_response.
                let serialize_result = {
                    let snapshot = ubos_config::handle().current();
                    let response = ubos_config::ConfigUpdate::from_config(&snapshot);
                    serde_json_core::ser::to_slice(&response, &mut body_buf)
                };
                let Ok(length) = serialize_result else {
                    warn!("config: failed to serialize ConfigUpdate response");
                    return write_status_only(
                        connection,
                        500,
                        "Internal Server Error",
                        "application/json",
                        &[],
                        b"{\"message\":\"config serialize failed\"}",
                    )
                    .await;
                };
                write_json_response(connection, 200, "OK", &body_buf[..length]).await
            }
            ("POST", "/api/config") => {
                info!("config update ({} bytes)", body.len());
                let Ok((mut update, _)) =
                    serde_json_core::de::from_slice::<ubos_config::ConfigUpdate>(body)
                else {
                    warn!("config: rejected POST /api/config with invalid JSON");
                    return write_status_only(
                        connection,
                        400,
                        "Bad Request",
                        "application/json",
                        &[],
                        b"{\"message\":\"invalid config update JSON\"}",
                    )
                    .await;
                };

                let config_handle = ubos_config::handle();
                let current = config_handle.current();

                // An empty password in the update means the operator did
                // not re-type the current secret; preserve the stored value
                // before validating so the printable-ASCII rule sees the
                // value that will actually be applied. A password loaded
                // from an older firmware build that bypassed validation
                // must not slip through an empty-password round-trip.
                if let Some(ref mut api) = update.api
                    && api.password.is_empty()
                {
                    api.password = current.api.password.clone();
                }

                if let Err(reason) = update.validate(&current, self.bounds) {
                    warn!("config: rejected POST /api/config: {}", reason);
                    // `reason` is always a `&'static str` literal from
                    // `ubos_config::ConfigUpdate::validate`; none contain
                    // `"` or `\`, so the formatter below produces valid JSON
                    // without escaping. Keep all new reasons static.
                    let mut error_buf = [0u8; 128];
                    let mut writer = ubos_http::BufferWriter::new(&mut error_buf);
                    let _ = core::write!(writer, "{{\"message\":\"{reason}\"}}");
                    let overflowed = writer.overflowed();
                    let error_len = writer.len();
                    let body: &[u8] = if overflowed {
                        warn!(
                            "config: error reason exceeded {}B error_buf; serving generic body",
                            error_buf.len()
                        );
                        b"{\"message\":\"config validation failed\"}"
                    } else {
                        &error_buf[..error_len]
                    };
                    return write_status_only(
                        connection,
                        400,
                        "Bad Request",
                        "application/json",
                        &[],
                        body,
                    )
                    .await;
                }

                let mut new_config = current;
                update.apply(&mut new_config);
                config_handle
                    .send(ubos_config::Message::Update(new_config))
                    .await;
                write_json_response(connection, 200, "OK", CONFIG_ACK_BODY).await
            }
            (_, "/api/info" | "/api/config") => {
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

/// Keep `CREDENTIALS` in sync with the config watch.
///
/// Seed the credential store from the persisted config before the accept loop.
///
/// Call once before spawning worker tasks so the first request
/// authenticates against the operator's configured credentials. Warn
/// when default credentials are still in place.
pub fn seed_api_credentials() {
    let config = ubos_config::handle();
    let api = config.current().api;
    ubos_auth::auth::set_credentials(&api.username, &api.password);
    ubos_auth::auth::warn_if_default_credentials();
}

/// Subscribe to config changes and update the credential store on each
/// publish so a `POST /api/config` credential change takes effect at
/// runtime without a reboot.
///
/// Run as a dedicated embassy task alongside the REST API worker tasks.
/// The future holds only the two credential strings across each
/// `set_credentials` await, not the full `Config` snapshot.
pub async fn run_sync_credentials() -> ! {
    let config = ubos_config::handle();
    let mut subscription = config.subscribe();
    loop {
        let credentials = {
            let snapshot = subscription.changed().await;
            snapshot.api
        };
        ubos_auth::auth::set_credentials(&credentials.username, &credentials.password);
    }
}

/// Task shell around [`run_sync_credentials`].
#[macro_export]
macro_rules! sync_credentials_task {
    () => {
        #[embassy_executor::task]
        async fn sync_credentials_task() -> ! {
            ubos_api::run_sync_credentials().await
        }
    };
}

/// Idle window before a chunk-ack upload session is treated as abandoned.
///
/// A chunk-ack upload that makes no progress for this long is treated as
/// abandoned; the watcher aborts it and resumes mining so a client that
/// vanished mid-stream does not strand the board stopped.
const OTA_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Reclaim abandoned chunk-ack upload sessions.
///
/// Spawned as a standalone task by OTA-capable boards alongside the pool
/// workers. Every [`OTA_SESSION_IDLE_TIMEOUT`] it checks whether a
/// chunk-ack upload is in progress and whether any new chunks have arrived
/// since the last tick. If the session is still in `Uploading` phase but
/// the chunk count has not advanced, the watcher tries to reclaim the
/// session: it takes the target mutex and — after re-checking under the
/// lock to avoid a race — aborts the target and resumes mining.
pub async fn run_ota_session_watchdog() -> ! {
    let mut last_seen_chunks: u32 = 0;
    loop {
        Timer::after(OTA_SESSION_IDLE_TIMEOUT).await;

        let snapshot = crate::ota::ota_session().snapshot();

        // Session is alive if it is not uploading or if new chunks arrived.
        if snapshot.phase != crate::ota::OtaPhase::Uploading || snapshot.chunks != last_seen_chunks
        {
            last_seen_chunks = snapshot.chunks;
            continue;
        }

        // Phase is Uploading and no progress was made for a full window.
        // Try to reclaim the session without blocking a live handler.
        let Ok(mut guard) = crate::ota::target_mutex().try_lock() else {
            // A handler currently owns the session and will clean up on its
            // own error path; leave it alone.
            continue;
        };

        // Re-check under the lock to avoid racing a chunk that was just acked.
        let snapshot = crate::ota::ota_session().snapshot();
        if snapshot.phase == crate::ota::OtaPhase::Uploading && snapshot.chunks == last_seen_chunks
        {
            if let Some(ota) = guard.as_deref_mut() {
                ota.abort();
            }
            crate::ota::ota_session().timeout();
            // Resume mining while still holding the target mutex. A fresh
            // offset-0 chunk arriving between this abort and the resume would
            // otherwise take the mutex, stop mining, and then be resumed by
            // this stale `Start`. Holding the guard until after the send
            // serializes the new session behind the resume.
            ubos_manager::handle().send(ManagerMessage::OtaEnd).await;
            drop(guard);
            warn!(
                "OTA: chunk session idle for {}s; aborting and resuming mining",
                OTA_SESSION_IDLE_TIMEOUT.as_secs(),
            );
        }

        last_seen_chunks = snapshot.chunks;
    }
}

/// Task shell around [`run_ota_session_watchdog`]. Only boards that
/// register an OTA target invoke it.
#[macro_export]
macro_rules! ota_watchdog_task {
    () => {
        #[embassy_executor::task]
        async fn ota_watchdog_task() -> ! {
            ubos_api::run_ota_session_watchdog().await
        }
    };
}
