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

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::TcpSplit;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use ubos_core::index::HashboardIndex;
use ubos_core::units::{FanPwm, FanRpm, Power, Voltage};
use ubos_logging::{info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_thermal::Handle as ThermalHandle;

use ubos_http::{
    RequestParts, Service, Status, read_body, write_json_response, write_message, write_status_only,
};

use crate::schema::info::{ApiInfo, ApiInfoChain, ApiInfoOta};
use crate::schema::state::{ApiMiningState, ApiPauseReason};

/// Whether the board is paused and the reason it gives, for both info
/// bodies. `dangerous` is the flag of thermal and `unreadable` is the
/// rail cut that mining makes when no temperature reading is left. The
/// two lead every reason the manager holds, because the manager holds
/// neither, and a board reads the same on either route.
pub(crate) fn pause_state(
    dangerous: bool,
    unreadable: bool,
    reasons: ubos_manager::PauseReasons,
) -> (bool, Option<ApiPauseReason>) {
    if dangerous {
        return (true, Some(ApiPauseReason::DangerousTemperature));
    }
    if unreadable {
        return (true, Some(ApiPauseReason::TemperatureUnreadable));
    }
    (
        !reasons.is_empty(),
        reasons.dominant().map(ApiPauseReason::from),
    )
}

#[cfg(test)]
#[path = "tests/rest.rs"]
mod tests;

/// Body buffer for `/api/info`. `api_info_body_fits_in_buffer` builds the
/// worst-case body, every numeric field at full width and the `chains`
/// array at `MAX_CHAINS`, and pins its length under this size, so a new
/// field that outgrows the buffer fails there instead of truncating the
/// tail.
const API_INFO_BODY_BUFFER_SIZE: usize = 1792;

const CONFIG_ACK_BODY: &[u8] = b"{\"ok\":true}";

/// Body buffer size for `GET /api/config` responses.
///
/// The widest [`ubos_config::ConfigUpdate`] snapshot the buffer has to
/// hold — the operator's three pool slots plus the fee entry and its
/// share the view carries beside them, each entry with 128-byte host
/// and user strings and an
/// SV1 password, a static `network` mode with three resolvers and a
/// 32-byte hostname, a 32-byte `api` username and password, and every
/// numeric field at its longest decimal form — serializes to 2617
/// bytes. The SV1 password is what makes a pool slot
/// widest: 128 bytes of string outruns the SV2 key, which travels as a
/// base58check string rather than as a JSON number array. That total
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
/// multiply by `HttpPoolSizes::http` for concurrent
/// connections. If you increase this constant, audit task-stack sizing for
/// all ubos-api consumers.
const CONFIG_RESPONSE_BUFFER_SIZE: usize = 3072;

/// Buffer for POST and PATCH bodies. Tracks [`CONFIG_RESPONSE_BUFFER_SIZE`]
/// so the widest body this API serves is one it accepts back. A larger
/// request answers 413.
const BODY_READ_BUFFER_SIZE: usize = CONFIG_RESPONSE_BUFFER_SIZE;

pub struct RestApiService {
    /// The control board's product name, e.g. `"W6100-EVB-Pico2"`.
    pub(crate) board_name: &'static str,
    /// The build id the board stamped, cut to
    /// [`crate::ota::MAX_BUILD_ID_LEN`] bytes.
    pub(crate) build: &'static str,
    pub(crate) mining_info: &'static ubos_config::MiningInfo,
    pub(crate) bounds: &'static ubos_config::MiningBounds,
    pub(crate) thermal: ThermalHandle,
    pub(crate) manager: ManagerHandle,
    pub(crate) psu: ubos_psu::Handle,
    pub(crate) cooling: ubos_cooling::Handle,
    pub(crate) mining: ubos_mining::Handle,
    pub(crate) net: ubos_net::net::Handle,
    pub(crate) config: ubos_config::Handle,
    pub(crate) stratum: stratum_client::PoolCountersHandle,
    pub(crate) stack: Stack<'static>,
}

impl RestApiService {
    /// The value goes into a `StaticCell` and every worker task takes the
    /// `&'static` reference.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        board_name: &'static str,
        build: &'static str,
        mining_info: &'static ubos_config::MiningInfo,
        bounds: &'static ubos_config::MiningBounds,
        thermal: ThermalHandle,
        manager: ManagerHandle,
        psu: ubos_psu::Handle,
        cooling: ubos_cooling::Handle,
        mining: ubos_mining::Handle,
        net: ubos_net::net::Handle,
        config: ubos_config::Handle,
        stratum: stratum_client::PoolCountersHandle,
        stack: Stack<'static>,
    ) -> Self {
        Self {
            board_name,
            build: crate::ota::truncate_build_id(build),
            mining_info,
            bounds,
            thermal,
            manager,
            psu,
            cooling,
            mining,
            net,
            config,
            stratum,
            stack,
        }
    }

    /// The `/api/info` snapshot, read from every subsystem the body
    /// reports. Each read is a lock-free snapshot, so an upload holding
    /// the OTA target mutex never blocks the dashboard's poll.
    fn info(&self) -> ApiInfo {
        let chains = self.mining.chains();
        let chain_readings = self.thermal.chains();
        let fan = self.cooling.current();
        let psu_reading = self.psu.current();
        let (paused, pause_reason) = pause_state(
            self.thermal.is_dangerous(),
            self.mining.temperature_unreadable(),
            self.manager.pause_reasons(),
        );
        let mining_state = self.mining.state();
        let ota = crate::ota::ota_info().map(|info| {
            ApiInfoOta::new(
                info,
                crate::ota::ota_session().snapshot(),
                crate::ota::reset_diag(),
            )
        });
        ApiInfo {
            name: self.board_name,
            hashrate: chains.machine_hashrate(),
            uptime: Instant::now().as_secs(),
            fan_rpm: fan.map_or_else(FanRpm::default, |f| f.rpm),
            fan_pwm: fan.map_or_else(FanPwm::default, |f| f.pwm),
            power_in: psu_reading.map_or_else(Power::default, |r| r.input_power),
            power_out: psu_reading.map_or_else(Power::default, |r| r.output_power),
            psu_voltage_set: psu_reading.map_or_else(Voltage::default, |r| r.output_voltage_set),
            psu_voltage_measured: psu_reading
                .map_or_else(Voltage::default, |r| r.output_voltage_measured),
            chain_voltage: chain_readings.lowest_voltage().unwrap_or_default(),
            psu_fault: psu_reading.is_some_and(|r| r.sensor_fault),
            pools: self.stratum.pool_alive_bitmap(),
            net_confirm_seconds: self.config.pending_seconds_remaining(),
            running: matches!(mining_state, Some(ubos_mining::MiningState::Running)),
            mining_state: mining_state.map(ApiMiningState::from),
            paused,
            pause_reason,
            // One entry per chain the board wired up. The snapshot holds
            // at most `MAX_CHAINS` of them, so `collect` cannot overflow
            // the vector.
            chains: chains
                .iter()
                .enumerate()
                .zip(HashboardIndex::range(chain_readings.slots()))
                .map(|((chain, stats), index)| {
                    ApiInfoChain::new(chain, stats, chain_readings.hottest(index))
                })
                .collect(),
            ip: crate::system::build_ip_string(self.net),
            mac: crate::system::build_mac_string(self.net),
            time: crate::system::build_time_string(self.net),
            build: self.build,
            product_name: crate::hw_info::product_name(),
            nominal_hashrate: crate::hw_info::nominal_hashrate(),
            ota,
        }
    }
}

impl Service for RestApiService {
    // A flat dispatch table; a helper per route would thread the generic
    // `Connection` through every call.
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
        let method = parts.method.as_str();
        let path = parts.path.as_str();
        let authorization = parts.authorization.as_deref();
        let cookie = parts.cookie.as_deref();
        let content_length = parts.content_length;

        // No anonymous endpoint exists under this prefix. CSRF is checked
        // after the body is read, so an oversized body does not burn the
        // session's token.
        let identity_policy = ubos_auth::Policy {
            allow_anonymous: false,
        };
        let auth_ctx =
            match ubos_auth::gate_identity(peer_ip, authorization, cookie, &identity_policy) {
                Err(ubos_auth::Rejection::NotAllowed) => {
                    return write_message(
                        connection,
                        Status::UNAUTHORIZED.with_message("Client not in allowed network range"),
                    )
                    .await;
                }
                Err(_) => {
                    // The challenge header goes with this answer, and
                    // `write_message` sends no header of the caller, so
                    // the body is written out here.
                    return write_status_only(
                        connection,
                        Status::UNAUTHORIZED,
                        "application/json",
                        &[("WWW-Authenticate", "Basic realm=\"ubos\"")],
                        b"{\"message\":\"unauthorized\"}",
                    )
                    .await;
                }
                Ok(auth_ctx) => auth_ctx,
            };

        let is_mutating = matches!(method, "POST" | "PATCH" | "DELETE");

        // Both OTA bodies exceed `body_buffer`; their handlers read the
        // connection themselves.
        let is_ota_upload = is_mutating && method == "POST" && path == "/api/system/OTA";

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

        // Only the offset-0 chunk, which starts a session, consumes a CSRF
        // token. A continuation is bound to the in-progress session by the
        // target mutex and the offset check, so a forged one can at most
        // corrupt an upload that then fails validation; it cannot start one.
        let is_ota_continuation = is_ota_chunk_upload
            && parts
                .ota_offset
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|offset| offset > 0);

        // Basic-authenticated requests are exempt: only a session cookie can
        // be forged cross-site. The identity resolved above is reused, so the
        // body read between the two cannot change who is checked.
        if is_mutating
            && !is_ota_continuation
            && ubos_auth::check_csrf(&auth_ctx, parts.csrf.as_deref()).is_err()
        {
            return write_message(
                connection,
                Status::FORBIDDEN.with_message("CSRF token invalid or missing"),
            )
            .await;
        }

        if is_ota_upload {
            return crate::system::handle_ota(
                connection,
                content_length,
                self.manager,
                self.mining,
                self.config,
            )
            .await;
        }

        if is_ota_chunk_upload {
            let ota_offset: u64 = match parts
                .ota_offset
                .as_deref()
                .and_then(|value| value.parse().ok())
            {
                Some(offset) => offset,
                None => {
                    return write_message(
                        connection,
                        Status::BAD_REQUEST.with_message("X-OTA-Offset header missing or invalid"),
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
                self.mining,
                self.config,
            )
            .await;
        }

        if path == "/api/system" || path.starts_with("/api/system/") {
            let handled = crate::system::dispatch(connection, self, method, path, body).await?;
            if !handled {
                return write_status_only(
                    connection,
                    Status::NOT_FOUND,
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
                let mut body_buf = [0u8; API_INFO_BODY_BUFFER_SIZE];
                // The snapshot is a temporary, so it does not live on the
                // future across the write.
                let serialize_result = serde_json_core::ser::to_slice(&self.info(), &mut body_buf);
                // Both outcomes share one await: a buffer held across two
                // awaits grows every HTTP worker by its size.
                let (status, body): (Status, &[u8]) = if let Ok(length) = serialize_result {
                    (Status::OK, &body_buf[..length])
                } else {
                    warn!("info: failed to serialize ApiInfo response");
                    (
                        Status::INTERNAL_SERVER_ERROR,
                        b"{\"message\":\"serialization error\"}",
                    )
                };
                write_json_response(connection, status, body).await
            }
            ("GET", "/api/config") => {
                // The view is read before the buffer exists, so the
                // future holds the buffer across the write only.
                let response = self.config.view().await;
                let mut body_buf = [0u8; CONFIG_RESPONSE_BUFFER_SIZE];
                // Drop the response struct before the await so it does
                // not live on the rest_api future stack across
                // write_json_response.
                let serialize_result = {
                    let response = response;
                    serde_json_core::ser::to_slice(&response, &mut body_buf)
                };
                let Ok(length) = serialize_result else {
                    warn!("config: failed to serialize ConfigUpdate response");
                    return write_message(
                        connection,
                        Status::INTERNAL_SERVER_ERROR.with_message("config serialize failed"),
                    )
                    .await;
                };
                write_json_response(connection, Status::OK, &body_buf[..length]).await
            }
            ("POST", "/api/config") => {
                info!("config update ({} bytes)", body.len());
                let Ok((mut update, _)) =
                    serde_json_core::de::from_slice::<ubos_config::ConfigUpdate>(body)
                else {
                    warn!("config: rejected POST /api/config with invalid JSON");
                    return write_message(
                        connection,
                        Status::BAD_REQUEST.with_message("invalid config update JSON"),
                    )
                    .await;
                };

                let current = self.config.current().await;

                // An empty password means the operator did not re-type the
                // current secret. It is restored before validation, so the
                // stored value is the one checked.
                if let Some(ref mut api) = update.api
                    && api.password.is_empty()
                {
                    api.password = current.api.password.clone();
                }

                if let Err(reason) = update.validate(&current, self.bounds) {
                    warn!("config: rejected POST /api/config: {}", reason);
                    return write_message(connection, Status::BAD_REQUEST.with_message(reason))
                        .await;
                }

                let mut new_config = current;
                update.apply(&mut new_config);
                self.config
                    .send(ubos_config::Message::Update(new_config))
                    .await;
                write_json_response(connection, Status::OK, CONFIG_ACK_BODY).await
            }
            (_, "/api/info" | "/api/config") => {
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

/// Seed the credential store from the persisted config. Called once
/// before the worker tasks, so the first request sees the operator's
/// credentials.
pub async fn seed_api_credentials(config: ubos_config::Handle) {
    let api = config.current().await.api;
    ubos_auth::auth::set_credentials(&api.username, &api.password);
    ubos_auth::auth::warn_if_default_credentials();
}

/// Update the credential store on each config publish, so a credential
/// change takes effect without a reboot. The future holds only the two
/// strings across the await, not the `Config` snapshot.
pub async fn run_sync_credentials(config: ubos_config::Handle) -> ! {
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
        async fn sync_credentials_task(config: ubos_config::Handle) -> ! {
            ubos_api::run_sync_credentials(config).await
        }
    };
}

/// A chunk-ack upload that makes no progress for this long is abandoned:
/// the watcher aborts it and resumes mining, so a vanished client does
/// not strand the board stopped.
const OTA_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Reclaim abandoned chunk-ack upload sessions; see
/// `OTA_SESSION_IDLE_TIMEOUT`.
pub async fn run_ota_session_watchdog(manager: ManagerHandle) -> ! {
    let mut last_seen_chunks: u32 = 0;
    loop {
        Timer::after(OTA_SESSION_IDLE_TIMEOUT).await;

        let snapshot = crate::ota::ota_session().snapshot();

        if snapshot.phase != crate::ota::OtaPhase::Uploading || snapshot.chunks != last_seen_chunks
        {
            last_seen_chunks = snapshot.chunks;
            continue;
        }

        let Ok(mut guard) = crate::ota::target_mutex().try_lock() else {
            // A handler owns the session and cleans up on its own error path.
            continue;
        };

        // The authoritative check is under the lock.
        let snapshot = crate::ota::ota_session().snapshot();
        if snapshot.phase == crate::ota::OtaPhase::Uploading && snapshot.chunks == last_seen_chunks
        {
            if let Some(ota) = guard.as_deref_mut() {
                ota.abort();
            }
            crate::ota::ota_session().timeout();
            // Sent under the target mutex, so a fresh offset-0 chunk cannot
            // stop mining between this abort and the resume and then be
            // resumed by it.
            manager.send(ManagerMessage::OtaEnd).await;
            drop(guard);
            warn!(
                "OTA: chunk session idle for {}s; aborting and resuming mining",
                OTA_SESSION_IDLE_TIMEOUT.as_secs(),
            );
        }

        last_seen_chunks = snapshot.chunks;
    }
}

/// Task shell around [`run_ota_session_watchdog`].
#[macro_export]
macro_rules! ota_watchdog_task {
    () => {
        #[embassy_executor::task]
        async fn ota_watchdog_task(manager: ubos_manager::Handle) -> ! {
            ubos_api::run_ota_session_watchdog(manager).await
        }
    };
}
