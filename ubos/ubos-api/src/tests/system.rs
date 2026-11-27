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

//! Tests for `system::route` and for the rejection paths of
//! `handle_ota_chunk`, driven through an in-memory mock socket.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can allocate buffers and use Vec.
extern crate std;

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Waker};
use std::vec::Vec;

use edge_nal::Readable;
use edge_nal::TcpSplit;
use embedded_io_async::{ErrorType, Read, Write};

use rusty_fork::rusty_fork_test;
use ubos_core::units::{FanPwm, FanRpm, Frequency, Hashrate, Shares, Voltage};
use ubos_manager::{Message, PauseReason, PauseReasons, run_manager};

use crate::ota::{OtaError, StagedConfig};
use crate::system::{DispatchOutcome, Route, route};

// ---------------------------------------------------------------------------
// In-memory mock socket
// ---------------------------------------------------------------------------

/// In-memory socket: the request bytes in, the response bytes out.
struct MockIo {
    read_data: Vec<u8>,
    read_pos: usize,
    write_buffer: Vec<u8>,
}

impl MockIo {
    fn new(request_bytes: Vec<u8>) -> Self {
        Self {
            read_data: request_bytes,
            read_pos: 0,
            write_buffer: Vec::new(),
        }
    }

    /// The status code of the written response.
    ///
    /// # Panics
    ///
    /// On a malformed status line, which is a fixture bug.
    fn response_status(&self) -> u16 {
        let status_str = std::str::from_utf8(&self.write_buffer[9..12])
            .expect("BUG: response bytes 9-11 are not UTF-8");
        status_str
            .parse()
            .expect("BUG: response bytes 9-11 are not a decimal status code")
    }
}

impl ErrorType for MockIo {
    type Error = core::convert::Infallible;
}

impl Read for MockIo {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let remaining = self.read_data.len().saturating_sub(self.read_pos);
        let count = remaining.min(buffer.len());
        buffer[..count].copy_from_slice(&self.read_data[self.read_pos..self.read_pos + count]);
        self.read_pos += count;
        Ok(count)
    }
}

impl Write for MockIo {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.write_buffer.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Readable for MockIo {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The handler never calls `split()`, so the halves only satisfy the
/// associated-type bounds.
struct NeverRead;

impl ErrorType for NeverRead {
    type Error = core::convert::Infallible;
}

impl Read for NeverRead {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        unreachable!("BUG: TcpSplit read half reached in test")
    }
}

impl Readable for NeverRead {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        unreachable!("BUG: TcpSplit readable reached in test")
    }
}

struct NeverWrite;

impl ErrorType for NeverWrite {
    type Error = core::convert::Infallible;
}

impl Write for NeverWrite {
    async fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
        unreachable!("BUG: TcpSplit write half reached in test")
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        unreachable!("BUG: TcpSplit flush half reached in test")
    }
}

impl TcpSplit for MockIo {
    type Read<'a>
        = NeverRead
    where
        Self: 'a;
    type Write<'a>
        = NeverWrite
    where
        Self: 'a;

    fn split(&mut self) -> (Self::Read<'_>, Self::Write<'_>) {
        panic!("BUG: TcpSplit::split called in OTA chunk test — unexpected code path");
    }
}

// ---------------------------------------------------------------------------
// Mock OtaTarget
// ---------------------------------------------------------------------------

/// `OtaTarget` mock with a configurable failure per method.
/// `initial_bytes_written` simulates a session already in progress.
struct MockOtaTarget {
    begin_result: Result<(), OtaError>,
    write_result: Result<(), OtaError>,
    finish_result: Result<(), OtaError>,
    bytes_received: usize,
    current_bytes_written: Option<usize>,
}

impl MockOtaTarget {
    fn succeeds() -> Self {
        Self {
            begin_result: Ok(()),
            write_result: Ok(()),
            finish_result: Ok(()),
            bytes_received: 0,
            current_bytes_written: None,
        }
    }

    fn with_bytes_written(written: usize) -> Self {
        Self {
            begin_result: Ok(()),
            write_result: Ok(()),
            finish_result: Ok(()),
            bytes_received: 0,
            current_bytes_written: Some(written),
        }
    }

    /// Mock that starts a session and fails every `write_chunk` with `error`.
    fn fails_writes(error: OtaError) -> Self {
        Self {
            write_result: Err(error),
            ..Self::succeeds()
        }
    }
}

impl crate::ota::OtaTarget for MockOtaTarget {
    fn bytes_written(&self) -> Option<usize> {
        self.current_bytes_written
    }

    fn running_version(&self) -> u32 {
        0
    }

    fn running_slot_label(&self) -> &'static str {
        "MOCK"
    }

    fn has_previous_image(&self) -> bool {
        false
    }

    fn previous_version(&self) -> Option<u32> {
        None
    }

    fn begin(&mut self) -> Result<(), OtaError> {
        if self.begin_result.is_ok() {
            self.current_bytes_written = Some(0);
        }
        self.begin_result
    }

    fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError> {
        if self.write_result.is_ok() {
            self.bytes_received += data.len();
            self.current_bytes_written = Some(self.current_bytes_written.unwrap_or(0) + data.len());
        }
        self.write_result
    }

    fn finish(&mut self) -> Result<(), OtaError> {
        self.finish_result
    }

    fn staged_config(&self) -> Option<StagedConfig> {
        None
    }

    fn reboot_into_new(&mut self) -> ! {
        panic!("BUG: reboot_into_new reached in test — final chunk without guard");
    }
}

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

/// A minimal `POST /api/system/OTA/chunk` request. Each entry of
/// `extra_headers` is a complete `Name: Value` header.
fn build_chunk_request(content_length: usize, body: &[u8], extra_headers: &[&str]) -> Vec<u8> {
    let mut request = Vec::new();
    request.extend_from_slice(b"POST /api/system/OTA/chunk HTTP/1.1\r\n");
    let cl_line = std::format!("Content-Length: {content_length}\r\n");
    request.extend_from_slice(cl_line.as_bytes());
    for header in extra_headers {
        request.extend_from_slice(header.as_bytes());
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);
    request
}

/// Drive `handle_ota_chunk` over a `MockIo` and return the status code.
/// The two content lengths are separate because the header must parse
/// for `Connection::new`, while the argument is what the REST layer
/// passed; `None` drives the absent-header path.
fn run_chunk_handler(
    content_length_header: usize,
    body: &[u8],
    extra_headers: &[&str],
    handler_content_length: Option<u64>,
    ota_offset: u64,
    ota_final: bool,
) -> u16 {
    // The test runs in a process of its own, so each handle is taken once.
    let (_manager_runner, manager) = ubos_manager::Handle::take();
    let (_mining_runner, mining) = ubos_mining::Handle::take();
    let (_config_runner, config) = ubos_config::Handle::take();

    let request_bytes = build_chunk_request(content_length_header, body, extra_headers);
    let io = MockIo::new(request_bytes);

    embassy_futures::block_on(async {
        let mut header_buf = [0u8; 512];
        let mut connection = edge_http::io::server::Connection::<_, 16>::new(&mut header_buf, io)
            .await
            .expect("BUG: Connection::new failed on mock HTTP request");

        crate::system::handle_ota_chunk(
            &mut connection,
            handler_content_length,
            ota_offset,
            ota_final,
            manager,
            mining,
            config,
        )
        .await
        .expect("BUG: handle_ota_chunk returned Err on mock socket — Infallible violated");

        connection
            .unbind()
            .expect("BUG: Connection::unbind after response")
            .response_status()
    })
}

/// Register `mock` as the OTA target and run `body`. Each test that
/// registers a target runs in a process of its own, so the static starts
/// empty and needs no reset. The mock is leaked to satisfy the
/// `&'static mut` the mutex holds.
fn with_ota_target<F: FnOnce() -> u16>(mock: MockOtaTarget, body: F) -> u16 {
    let leaked: &'static mut dyn crate::ota::OtaTarget =
        std::boxed::Box::leak(std::boxed::Box::new(mock));
    embassy_futures::block_on(async {
        *crate::ota::target_mutex().lock().await = Some(leaked);
    });

    body()
}

// ---------------------------------------------------------------------------
// Route tests (unchanged)
// ---------------------------------------------------------------------------

/// Each row covers the accepted method and the other-method rejection.
const ROUTES: &[(&str, &str, Route)] = &[
    ("GET", "/api/system/info", Route::SystemInfo),
    ("GET", "/api/system/asic", Route::SystemAsic),
    ("GET", "/api/system/thermal", Route::SystemThermal),
    ("GET", "/api/system/temperature", Route::SystemTemperature),
    ("GET", "/api/system/cooling", Route::SystemCooling),
    ("GET", "/api/system/statistics", Route::SystemStatistics),
    ("GET", "/api/system/scoreboard", Route::SystemScoreboard),
    ("GET", "/api/system/events", Route::SystemEvents),
    (
        "GET",
        "/api/system/events/catalog",
        Route::SystemEventsCatalog,
    ),
    ("GET", "/api/system/wifi/scan", Route::SystemWifiScan),
    ("POST", "/api/system/OTA", Route::SystemOta),
    ("POST", "/api/system/OTA/chunk", Route::SystemOtaChunk),
    ("POST", "/api/system/OTAWWW", Route::SystemOtaWww),
    ("POST", "/api/system/pause", Route::PauseAck),
    ("POST", "/api/system/resume", Route::ResumeAck),
    ("POST", "/api/system/thermal/clear", Route::ThermalClearAck),
    ("POST", "/api/system/restart", Route::RestartAck),
    ("POST", "/api/system/identify", Route::IdentifyAck),
    (
        "POST",
        "/api/system/blockFound/dismiss",
        Route::BlockFoundDismissAck,
    ),
    ("POST", "/api/system/factory_reset", Route::FactoryReset),
    ("PATCH", "/api/system", Route::PatchSystem),
    ("POST", "/api/system/network/confirm", Route::NetworkConfirm),
    ("POST", "/api/system/network/revert", Route::NetworkRevert),
];

#[test]
fn route_table_handles_matching_method_and_rejects_others() {
    // The alternative verb always differs from the row's, so it exercises
    // the 405 path.
    for (allowed, path, expected) in ROUTES {
        assert_eq!(
            route(allowed, path),
            DispatchOutcome::Handled(*expected),
            "{allowed} {path} should be Handled",
        );
        let wrong = match *allowed {
            "GET" => "POST",
            "POST" | "PATCH" => "GET",
            other => panic!("test table needs an alternative for method {other}"),
        };
        assert_eq!(
            route(wrong, path),
            DispatchOutcome::MethodNotAllowed { allowed },
            "{wrong} {path} should be MethodNotAllowed (allowed={allowed})",
        );
    }
}

#[test]
fn route_network_dispatches_both_methods() {
    // Two accepted methods, so it cannot live in the single-method table.
    assert_eq!(
        route("GET", "/api/system/network"),
        DispatchOutcome::Handled(Route::NetworkGet),
    );
    assert_eq!(
        route("POST", "/api/system/network"),
        DispatchOutcome::Handled(Route::NetworkPost),
    );
    assert_eq!(
        route("PATCH", "/api/system/network"),
        DispatchOutcome::MethodNotAllowed {
            allowed: "GET, POST",
        },
    );
}

#[test]
fn route_unknown_system_subpath_not_found() {
    assert_eq!(
        route("GET", "/api/system/unknown"),
        DispatchOutcome::NotFound
    );
}

#[test]
fn route_api_system_trailing_slash_not_found() {
    assert_eq!(route("GET", "/api/system/"), DispatchOutcome::NotFound);
}

#[test]
fn route_root_not_found() {
    assert_eq!(route("GET", "/"), DispatchOutcome::NotFound);
}

#[test]
fn route_api_info_not_found() {
    assert_eq!(route("GET", "/api/info"), DispatchOutcome::NotFound);
}

#[test]
fn route_query_string_stripped_before_matching() {
    assert_eq!(
        route("GET", "/api/system/events?since=5"),
        DispatchOutcome::Handled(Route::SystemEvents),
        "query string must be stripped before matching the events route",
    );
}

#[test]
fn parse_since_absent_query_is_zero() {
    assert_eq!(crate::system::parse_since("/api/system/events"), 0);
}

#[test]
fn parse_since_missing_key_is_zero() {
    assert_eq!(crate::system::parse_since("/api/system/events?foo=1"), 0);
}

#[test]
fn parse_since_reads_decimal_value() {
    assert_eq!(
        crate::system::parse_since("/api/system/events?since=42"),
        42
    );
}

#[test]
fn parse_since_among_other_params() {
    assert_eq!(
        crate::system::parse_since("/api/system/events?foo=1&since=7&bar=2"),
        7,
    );
}

#[test]
fn parse_since_non_decimal_is_zero() {
    assert_eq!(
        crate::system::parse_since("/api/system/events?since=abc"),
        0
    );
}

#[test]
fn parse_since_overflow_is_max() {
    assert_eq!(
        crate::system::parse_since("/api/system/events?since=4294967296"),
        u32::MAX,
    );
}

// ---------------------------------------------------------------------------
// OTA chunk edge-case tests
// ---------------------------------------------------------------------------

rusty_fork_test! {
    #[test]
    fn ota_chunk_oversize_returns_413() {
        let oversize: u64 = 4097;
        let status = run_chunk_handler(1, &[0u8], &[], Some(oversize), 0, false);
        assert_eq!(status, 413, "oversize chunk must be rejected with 413");
    }
}

rusty_fork_test! {
    #[test]
    fn ota_chunk_exceeds_image_max_returns_413() {
        let offset_at_cap: u64 = 952 * 1024;
        let status = run_chunk_handler(1, &[0u8], &[], Some(1), offset_at_cap, false);
        assert_eq!(
            status, 413,
            "chunk that would push image past slot cap must be rejected with 413"
        );
    }
}

rusty_fork_test! {
    // A continuation chunk whose `X-OTA-Offset` does not match the number of
    // bytes already written returns 409 Conflict, and the session ends with
    // that error, so the idle watcher does not abort it a second time.
    //
    // Simulated by pre-registering a mock whose `bytes_written()` reports
    // 4096 (one sector already written), then sending a chunk with
    // `ota_offset = 512` (neither 0 nor 4096).
    #[test]
    fn ota_chunk_offset_mismatch_returns_409() {
        let status = with_ota_target(MockOtaTarget::with_bytes_written(4096), || {
            run_chunk_handler(512, &[0u8; 512], &[], Some(512), 512, false)
        });
        assert_eq!(
            status, 409,
            "chunk with mismatched offset must be rejected with 409"
        );
        let session = crate::ota::ota_session().snapshot();
        assert_eq!(session.phase, crate::ota::OtaPhase::Error);
        assert_eq!(session.last_error, OtaError::OffsetMismatch as u8);
    }
}

rusty_fork_test! {
    #[test]
    fn ota_chunk_replayed_offset_returns_409() {
        // Offset 0 would restart the session, so the replay uses a later one.
        let status = with_ota_target(MockOtaTarget::with_bytes_written(1024), || {
            run_chunk_handler(256, &[0u8; 256], &[], Some(256), 256, false)
        });
        assert_eq!(
            status, 409,
            "replayed (already-acked) chunk must be rejected with 409"
        );
    }
}

rusty_fork_test! {
    #[test]
    fn ota_chunk_final_image_too_small_returns_400() {
        let status = with_ota_target(MockOtaTarget::succeeds(), || {
            run_chunk_handler(128, &[0u8; 128], &[], Some(128), 0, true)
        });
        assert_eq!(
            status, 400,
            "final chunk that would produce a sub-512-byte image must be rejected with 400"
        );
    }
}

rusty_fork_test! {
    // A non-final chunk shorter than one erase sector is rejected with 400,
    // and the session ends with that error.
    //
    // `write_chunk` assumes each non-final chunk covers exactly one sector, so
    // a short body would advance the write offset off a sector boundary. The
    // guard fires after the offset-continuity check, so the session is armed:
    // `bytes_written = 4096` with a matching `ota_offset = 4096` reaches it.
    #[test]
    fn ota_chunk_non_final_short_returns_400() {
        // The offset matches, so the guard after the continuity check fires.
        let status = with_ota_target(MockOtaTarget::with_bytes_written(4096), || {
            run_chunk_handler(512, &[0u8; 512], &[], Some(512), 4096, false)
        });
        assert_eq!(
            status, 400,
            "non-final chunk smaller than one sector must be rejected with 400"
        );
        let session = crate::ota::ota_session().snapshot();
        assert_eq!(session.phase, crate::ota::OtaPhase::Error);
        assert_eq!(session.last_error, OtaError::ShortChunk as u8);
    }
}

rusty_fork_test! {
    // A first chunk that the target refuses as an image for other hardware
    // is answered with 400, and the session records that error. A client
    // then tells the wrong file apart from a fault of the board.
    #[test]
    fn ota_chunk_wrong_hardware_returns_400() {
        let chunk = [0u8; crate::system::OTA_CHUNK_SIZE];
        let status = with_ota_target(MockOtaTarget::fails_writes(OtaError::WrongHardware), || {
            run_chunk_handler(chunk.len(), &chunk, &[], Some(chunk.len() as u64), 0, false)
        });
        assert_eq!(status, 400, "an image for other hardware must be rejected with 400");
        assert_eq!(
            crate::ota::ota_session().snapshot().last_error,
            OtaError::WrongHardware as u8,
        );
    }
}

rusty_fork_test! {
    // A chunk that the target refuses because the image overflows its slot
    // is answered with 413, because the client sent a file that the board
    // cannot hold.
    #[test]
    fn ota_chunk_slot_overflow_returns_413() {
        let chunk = [0u8; crate::system::OTA_CHUNK_SIZE];
        let status = with_ota_target(MockOtaTarget::fails_writes(OtaError::SlotOverflow), || {
            run_chunk_handler(chunk.len(), &chunk, &[], Some(chunk.len() as u64), 0, false)
        });
        assert_eq!(status, 413, "an image too big for the slot must be rejected with 413");
    }
}

rusty_fork_test! {
    // A flash error from `write_chunk` stays a 500, because it is a fault
    // of the board and not of the image.
    #[test]
    fn ota_chunk_flash_error_returns_500() {
        let chunk = [0u8; crate::system::OTA_CHUNK_SIZE];
        let status = with_ota_target(MockOtaTarget::fails_writes(OtaError::Flash), || {
            run_chunk_handler(chunk.len(), &chunk, &[], Some(chunk.len() as u64), 0, false)
        });
        assert_eq!(status, 500, "a flash error must be answered with 500");
    }
}

// ---------------------------------------------------------------------------
// SystemInfo paused / pause_reason serialization
// ---------------------------------------------------------------------------

/// Build a default `HwInfo` for tests that do not exercise hardware
/// version fields: no PSU model and no published brand or product name,
/// nominal hashrate, versions or hashboard serials.
fn default_hw_info() -> crate::schema::system_info::HwInfo {
    crate::schema::system_info::HwInfo {
        brand_name: None,
        product_name: None,
        nominal_hashrate: None,
        bootloader_version: None,
        fan_controller_version: None,
        psu_model: None,
        hashboard_ids: None,
        chains_present: 0,
    }
}

/// A minimal `SystemInfo`, serialized.
fn serialize_system_info(
    paused: bool,
    pause_reason: Option<crate::schema::state::ApiPauseReason>,
    hw_info: crate::schema::system_info::HwInfo,
) -> std::string::String {
    serialize_system_info_with_ota(paused, pause_reason, hw_info, "test", None)
}

fn serialize_system_info_with_ota(
    paused: bool,
    pause_reason: Option<crate::schema::state::ApiPauseReason>,
    hw_info: crate::schema::system_info::HwInfo,
    build: &'static str,
    ota: Option<crate::schema::system_info::OtaInfo>,
) -> std::string::String {
    let info = crate::schema::SystemInfo {
        api_variant: crate::schema::API_VARIANT,
        board_name: "test",
        build,
        hostname: "test",
        hashrate: Hashrate::default(),
        uptime_seconds: 0,
        fan_speed: FanRpm::from_rpm(u32::MAX),
        fan_pwm: FanPwm::FULL,
        pools_bitmap: 0,
        ip: heapless::String::new(),
        time: heapless::String::new(),
        asic_count: 0,
        asic_model: "BM1362",
        default_frequency: Frequency::from_khz(u32::MAX),
        default_voltage: Voltage::from_mv(u32::MAX),
        shares_accepted: 0,
        shares_rejected: 0,
        shares_total: Shares::default(),
        best_nonce_diff: 0,
        best_session_nonce_diff: 0,
        ota,
        // The widest state string, so the worst-case measurement below
        // covers it.
        mining_state: Some(crate::schema::state::ApiMiningState::WaitingForPool),
        paused,
        pause_reason,
        hw_info,
    };
    let mut buf = [0u8; super::SCRATCH_SIZE];
    let len = serde_json_core::to_slice(&info, &mut buf).expect("BUG: SystemInfo serialize failed");
    std::string::String::from_utf8(buf[..len].to_vec())
        .expect("BUG: SystemInfo output is not UTF-8")
}

#[test]
fn system_info_running_serializes_paused_false_and_null_reason() {
    let json = serialize_system_info(false, None, default_hw_info());
    assert!(
        json.contains("\"paused\":false"),
        "expected paused:false in {json}"
    );
    assert!(
        json.contains("\"pauseReason\":null"),
        "expected pauseReason:null in {json}"
    );
}

#[test]
fn system_info_units_serialize_as_integers_under_their_names() {
    let json = serialize_system_info(false, None, default_hw_info());
    for expected in [
        "\"fanSpeedRpm\":4294967295",
        "\"fanPwmPercent\":100",
        "\"defaultFrequencyKhz\":4294967295",
        "\"defaultVoltageMv\":4294967295",
    ] {
        assert!(json.contains(expected), "expected {expected} in {json}");
    }
}

#[test]
fn system_info_user_request_serializes_paused_true_and_reason() {
    let json = serialize_system_info(
        true,
        Some(crate::schema::state::ApiPauseReason::UserRequest),
        default_hw_info(),
    );
    assert!(
        json.contains("\"paused\":true"),
        "expected paused:true in {json}"
    );
    assert!(
        json.contains("\"pauseReason\":\"userRequest\""),
        "expected pauseReason:\"userRequest\" in {json}"
    );
}

// ---------------------------------------------------------------------------
// SystemInfo hwInfo serialization
// ---------------------------------------------------------------------------

#[test]
fn system_info_hw_info_serializes_published_versions() {
    let mut hashboard_ids: crate::hw_info::HashboardIds = heapless::Vec::new();
    hashboard_ids
        .push(Some("SERIAL0".parse().expect("BUG: fits in HwString")))
        .expect("BUG: fits in HashboardIds");
    hashboard_ids.push(None).expect("BUG: fits in HashboardIds");
    hashboard_ids
        .push(Some("SERIAL2".parse().expect("BUG: fits in HwString")))
        .expect("BUG: fits in HashboardIds");

    let hw_info = crate::schema::system_info::HwInfo {
        brand_name: Some("Example Brand".parse().expect("BUG: fits in HwString")),
        product_name: Some("Example Miner 1".parse().expect("BUG: fits in HwString")),
        nominal_hashrate: Some(175_000_000_000_000),
        bootloader_version: Some("deadbeef".parse().expect("BUG: fits in HwString")),
        fan_controller_version: Some("12.34.56 build 777".parse().expect("BUG: fits in HwString")),
        psu_model: Some("PSU3300 rev a".parse().expect("BUG: fits in HwString")),
        hashboard_ids: Some(hashboard_ids),
        chains_present: 2,
    };
    let json = serialize_system_info(false, None, hw_info);
    assert!(
        json.contains(
            "\"hwInfo\":{\"brandName\":\"Example Brand\",\"productName\":\"Example Miner 1\",\
             \"nominalHashrate\":175000000000000,\"bootloaderVersion\":\"deadbeef\",\
             \"fanControllerVersion\":\"12.34.56 build 777\",\"psuModel\":\"PSU3300 rev a\",\
             \"hashboardIds\":[\"SERIAL0\",null,\"SERIAL2\"],\"chainsPresent\":2}"
        ),
        "expected populated hwInfo object in {json}"
    );
}

/// Pinned exactly, so a field addition that still fits the buffer is
/// noticed here; the `const` block keeps it under `SCRATCH_SIZE`.
const WORST_CASE_SYSTEM_INFO_LEN: usize = 1619;
const _: () = assert!(WORST_CASE_SYSTEM_INFO_LEN <= super::SCRATCH_SIZE);

// The scratch buffer `system.rs` serializes into must hold the largest
// `SystemInfo` the board can produce, and the helper above would panic
// before this test's assertion if it did not; what this test adds is
// the exact length.
//
// Every string field stands at its maximum, and the build id is
// `MAX_BUILD_ID_LEN` double quotes, the widest character a build id
// can carry: the build environment can hand the id whole with a quote
// or a backslash in it. The brand and product
// names are `HW_STRING_LEN` control characters, which serialize to six
// bytes each, and the nominal hashrate is `u64::MAX`.
//
// Without this, nothing measures the body and the comment on
// `SCRATCH_SIZE` is the only thing holding its size.
#[test]
fn worst_case_system_info_fits_the_scratch_buffer() {
    use crate::hw_info::HW_STRING_LEN;
    use crate::ota::MAX_BUILD_ID_LEN;

    /// Leaked, since the schema takes `&'static str`.
    fn leaked(byte: u8, len: usize) -> &'static str {
        let text = std::string::String::from_utf8(std::vec![byte; len])
            .expect("BUG: ASCII fill is not UTF-8");
        std::boxed::Box::leak(text.into_boxed_str())
    }

    let max_string: crate::hw_info::HwString = leaked(b'x', HW_STRING_LEN)
        .parse()
        .expect("BUG: fills HwString exactly");
    let mut hashboard_ids = crate::hw_info::HashboardIds::new();
    while hashboard_ids.push(Some(max_string.clone())).is_ok() {}

    let hw_info = crate::schema::system_info::HwInfo {
        brand_name: Some(
            leaked(0x01, HW_STRING_LEN)
                .parse()
                .expect("BUG: fills HwString exactly"),
        ),
        product_name: Some(
            leaked(0x01, HW_STRING_LEN)
                .parse()
                .expect("BUG: fills HwString exactly"),
        ),
        nominal_hashrate: Some(u64::MAX),
        bootloader_version: Some(max_string.clone()),
        fan_controller_version: Some(max_string.clone()),
        psu_model: Some(max_string),
        hashboard_ids: Some(hashboard_ids),
        chains_present: u8::MAX,
    };

    let ota = crate::schema::system_info::OtaInfo {
        running_slot: "INACTIVE",
        running_version: u32::MAX,
        has_previous_image: true,
        previous_version: Some(u32::MAX),
        max_image_bytes: u64::MAX,
    };

    let json = serialize_system_info_with_ota(
        true,
        Some(crate::schema::state::ApiPauseReason::TemperatureUnreadable),
        hw_info,
        leaked(b'"', MAX_BUILD_ID_LEN),
        Some(ota),
    );
    assert_eq!(
        json.len(),
        WORST_CASE_SYSTEM_INFO_LEN,
        "the worst-case SystemInfo moved; check the comment on SCRATCH_SIZE"
    );
}

// The temperature body names its fields in camelCase, writes a reading
// the sample did not take as `null`, and at its widest, every value at
// `i32::MIN`, fits the scratch buffer.
#[test]
fn system_temperature_body_at_its_widest() {
    let body = crate::schema::SystemTemperature {
        chip_max: Some(i32::MIN),
        chip_mean: Some(i32::MIN),
        hashboard_max: Some(i32::MIN),
        hashboard_mean: None,
        ambient: Some(i32::MIN),
        pid_source: Some(ubos_config::Source::Hashboard),
    };
    let mut buf = [0u8; super::SCRATCH_SIZE];
    let len = serde_json_core::to_slice(&body, &mut buf)
        .expect("BUG: SystemTemperature serialize failed");
    assert_eq!(
        core::str::from_utf8(&buf[..len]),
        Ok("{\"chipMax\":-2147483648,\"chipMean\":-2147483648,\
            \"hashboardMax\":-2147483648,\"hashboardMean\":null,\
            \"ambient\":-2147483648,\"pidSource\":\"hashboard\"}")
    );
}

#[test]
fn system_info_hw_info_serializes_unpublished_versions_as_null() {
    let json = serialize_system_info(false, None, default_hw_info());
    assert!(
        json.contains(
            "\"productName\":null,\"nominalHashrate\":null,\"bootloaderVersion\":null,\
             \"fanControllerVersion\":null,\"psuModel\":null,\"hashboardIds\":null,\
             \"chainsPresent\":0"
        ),
        "expected null productName, nominalHashrate, bootloaderVersion, fanControllerVersion, \
         and hashboardIds in {json}"
    );
}

// ---------------------------------------------------------------------------
// Resume tests
// ---------------------------------------------------------------------------

/// Run `body` against a manager that runs as the firmware runs it, after it
/// handled `prime`. `settle` lets the manager handle what the body sent.
fn with_manager<R>(
    prime: &[Message],
    body: impl FnOnce(ubos_manager::Handle, &mut dyn FnMut()) -> R,
) -> R {
    let (mut runner, manager) = ubos_manager::Handle::take();
    let (_mining_runner, mining) = ubos_mining::Handle::take();
    let mut task = pin!(run_manager(&mut runner, mining));
    let mut settle = || {
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..SETTLE_POLLS {
            let _ = task.as_mut().poll(&mut cx);
        }
    };
    embassy_futures::block_on(async {
        for message in prime {
            manager.send(*message).await;
        }
    });
    settle();
    body(manager, &mut settle)
}

/// Polls that let the manager handle every message that waits for it.
const SETTLE_POLLS: usize = 16;

/// Drive `handle_resume` over a mock socket against `manager` and return
/// the HTTP status code from the response.
///
/// The caller gets `manager` from [`with_manager`], so the mask the handler
/// reads is the one the real manager published.
fn run_resume_handler(manager: ubos_manager::Handle) -> u16 {
    let io = MockIo::new(b"POST /api/system/resume HTTP/1.1\r\nContent-Length: 0\r\n\r\n".to_vec());

    embassy_futures::block_on(async {
        let mut header_buf = [0u8; 512];
        let mut connection = edge_http::io::server::Connection::<_, 16>::new(&mut header_buf, io)
            .await
            .expect("BUG: Connection::new failed on mock HTTP request");

        crate::system::handle_resume(&mut connection, manager)
            .await
            .expect("BUG: handle_resume returned Err on mock socket — Infallible violated");

        connection
            .unbind()
            .expect("BUG: Connection::unbind after response")
            .response_status()
    })
}

rusty_fork_test! {
    #[test]
    fn resume_during_overheat_halt_is_refused() {
        with_manager(&[Message::DangerousTemperature], |manager, settle| {
            let status = run_resume_handler(manager);
            settle();

            assert_eq!(status, 409);
            assert_eq!(
                manager.pause_reasons(),
                PauseReasons::default().assert(PauseReason::DangerousTemperature),
                "a refused resume must change nothing in the manager"
            );
        });
    }

    #[test]
    fn resume_without_overheat_halt_starts() {
        with_manager(&[Message::Stop], |manager, settle| {
            let status = run_resume_handler(manager);
            settle();

            assert_eq!(status, 200);
            assert!(
                manager.pause_reasons().is_empty(),
                "an accepted resume must clear the operator's pause"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Thermal-clear tests
// ---------------------------------------------------------------------------

/// Drive `handle_thermal_clear` over a mock socket against `manager` and
/// return the HTTP status code from the response.
///
/// Built the same way as `run_resume_handler`, and for the same reason.
fn run_thermal_clear_handler(manager: ubos_manager::Handle) -> u16 {
    let io = MockIo::new(
        b"POST /api/system/thermal/clear HTTP/1.1\r\nContent-Length: 0\r\n\r\n".to_vec(),
    );

    embassy_futures::block_on(async {
        let mut header_buf = [0u8; 512];
        let mut connection = edge_http::io::server::Connection::<_, 16>::new(&mut header_buf, io)
            .await
            .expect("BUG: Connection::new failed on mock HTTP request");

        crate::system::handle_thermal_clear(&mut connection, manager)
            .await
            .expect("BUG: handle_thermal_clear returned Err on mock socket — Infallible violated");

        connection
            .unbind()
            .expect("BUG: Connection::unbind after response")
            .response_status()
    })
}

rusty_fork_test! {
    #[test]
    fn thermal_clear_acknowledges_then_starts() {
        with_manager(&[Message::DangerousTemperature], |manager, settle| {
            let status = run_thermal_clear_handler(manager);
            settle();

            assert_eq!(status, 200);
            assert!(
                manager.pause_reasons().is_empty(),
                "the clear must lift the halt and leave nothing paused"
            );
        });
    }

    #[test]
    fn thermal_clear_starts_whether_or_not_the_operator_paused() {
        // The operator's own pause on top of the halt. Clearing is the same
        // two sends either way, which is what keeps one confirm enough.
        with_manager(
            &[Message::DangerousTemperature, Message::Stop],
            |manager, settle| {
                let status = run_thermal_clear_handler(manager);
                settle();

                assert_eq!(status, 200);
                assert!(
                    manager.pause_reasons().is_empty(),
                    "the clear must lift the halt and the operator's pause with it"
                );
            },
        );
    }
}

// The system info schema carries the camelCase keys the device
// serializes, so a client generated from it reads the live body.
#[cfg(feature = "schema")]
#[test]
fn system_info_schema_uses_camel_case() {
    let schema = schemars::schema_for!(crate::schema::SystemInfo);
    let value = serde_json::to_value(&schema).expect("schema is JSON");
    let properties = value["properties"].as_object().expect("root is an object");
    for key in ["apiVariant", "hwInfo", "uptimeSeconds", "pauseReason"] {
        assert!(properties.contains_key(key), "missing key {key}");
    }
    let hw = &value["$defs"]["HwInfo"]["properties"]["hashboardIds"];
    assert_eq!(hw["type"], serde_json::json!(["array", "null"]));
}

// --- event bodies serialize from structs ---

#[test]
fn event_record_serializes_the_documented_bytes() {
    let record = ubos_events::EventRecord {
        seq: 7,
        ts: 99,
        code: 0xdead_beef,
        severity: 1,
        args: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
    };
    let json = crate::schema::events::EventRecordJson::from(&record);
    let mut buffer = [0u8; 128];
    let len = serde_json_core::to_slice(&json, &mut buffer).expect("record fits");
    assert_eq!(
        std::str::from_utf8(&buffer[..len]).expect("valid UTF-8"),
        "{\"seq\":7,\"ts\":99,\"sev\":\"warn\",\"code\":3735928559,\
         \"args\":\"000102030405060708090a\"}"
    );
}

#[test]
fn event_def_serializes_the_documented_bytes() {
    let def = ubos_events::EventDef {
        code: 5,
        severity: ubos_events::Severity::Error,
        fmt: "say \"hi\" \\ now",
        args: &[ubos_events::ArgWidth::U8, ubos_events::ArgWidth::I32],
    };
    let json = crate::schema::events::EventDefJson::from(&def);
    let mut buffer = [0u8; 256];
    let len = serde_json_core::to_slice(&json, &mut buffer).expect("def fits");
    assert_eq!(
        std::str::from_utf8(&buffer[..len]).expect("valid UTF-8"),
        "{\"code\":5,\"sev\":\"error\",\"fmt\":\"say \\\"hi\\\" \\\\ now\",\
         \"args\":[\"u8\",\"i32\"]}"
    );
}

// A catalog entry wider than its buffer fails to serialize, which is
// the path that skips it and keeps the streamed array valid.
#[test]
fn oversized_event_def_does_not_fit_its_buffer() {
    let fmt: &'static str =
        std::boxed::Box::leak("\"".repeat(ubos_events::MAX_FMT_LEN).into_boxed_str());
    let def = ubos_events::EventDef {
        code: 1,
        severity: ubos_events::Severity::Info,
        fmt,
        args: &[],
    };
    let mut buffer = [0u8; super::DEF_JSON_MAX];
    assert!(
        serde_json_core::to_slice(
            &crate::schema::events::EventDefJson::from(&def),
            &mut buffer
        )
        .is_err()
    );
}

#[test]
fn event_severity_serializes_lowercase() {
    let mut buf = [0u8; 8];
    for (severity, expected) in [
        (ubos_events::Severity::Info, "\"info\""),
        (ubos_events::Severity::Warn, "\"warn\""),
        (ubos_events::Severity::Error, "\"error\""),
    ] {
        let n = serde_json_core::to_slice(
            &crate::schema::events::ApiSeverity::from(severity),
            &mut buf,
        )
        .expect("fits");
        assert_eq!(core::str::from_utf8(&buf[..n]).expect("utf8"), expected);
    }
}

// The streamed body is the envelope struct's serialization: the
// handler writes the opening, the comma and the closing as literals,
// and this holds them to the struct the document describes.
#[test]
fn streamed_events_body_matches_the_envelope_struct() {
    let record = ubos_events::EventRecord {
        seq: 1,
        ts: 2,
        code: 3,
        severity: 0,
        args: [0; 11],
    };
    let entries = [
        crate::schema::events::EventRecordJson::from(&record),
        crate::schema::events::EventRecordJson::from(&record),
    ];
    let mut whole = [0u8; 256];
    let whole_len = serde_json_core::to_slice(
        &crate::schema::events::EventsResponse { events: &entries },
        &mut whole,
    )
    .expect("fits");
    let mut one = [0u8; 128];
    let one_len = serde_json_core::to_slice(&entries[0], &mut one).expect("fits");
    let mut streamed = std::vec::Vec::new();
    streamed.extend_from_slice(super::EVENTS_OPEN);
    streamed.extend_from_slice(&one[..one_len]);
    streamed.extend_from_slice(b",");
    streamed.extend_from_slice(&one[..one_len]);
    streamed.extend_from_slice(super::EVENTS_CLOSE);
    assert_eq!(&whole[..whole_len], streamed.as_slice());
}

// --- Route names its own method and path ---

#[test]
fn every_route_round_trips() {
    for (i, r) in Route::ALL.iter().enumerate() {
        let (method, path) = r.endpoint();
        assert_eq!(
            route(method, path),
            DispatchOutcome::Handled(*r),
            "{method} {path} does not reach {r:?}"
        );
        assert!(
            !Route::ALL[..i].contains(r),
            "{r:?} is listed twice in Route::ALL"
        );
    }
}
