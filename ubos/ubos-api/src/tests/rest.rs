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

//! Tests for REST-specific helpers: `validate_board_name` and the
//! `/api/info` and `/api/config` body-buffer sizes.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

use core::fmt::Write as _;

use crate::rest::{API_INFO_BODY_BUFFER_SIZE, CONFIG_RESPONSE_BUFFER_SIZE, validate_board_name};

// --- M7: validate_board_name ---

/// Board names with no special characters must pass the check.
#[test]
fn validate_board_name_accepts_safe_names() {
    assert!(validate_board_name("W5500-EVB-Pico2"));
    assert!(validate_board_name("host-runner"));
    assert!(validate_board_name(""));
    assert!(validate_board_name("abc123 !@#$%^&*()"));
}

/// A board name containing a double-quote must be rejected.
#[test]
fn validate_board_name_rejects_double_quote() {
    assert!(!validate_board_name("bad\"name"));
    assert!(!validate_board_name("\"starts"));
    assert!(!validate_board_name("ends\""));
}

/// A board name containing a backslash must be rejected.
#[test]
fn validate_board_name_rejects_backslash() {
    assert!(!validate_board_name("bad\\name"));
    assert!(!validate_board_name("\\starts"));
    assert!(!validate_board_name("ends\\"));
}

// --- N6: API_INFO_BODY_BUFFER_SIZE covers the worst-case body ---

/// Measured length of the worst-case `/api/info` body.
///
/// Pinned exactly, as [`WORST_CASE_BODY_LEN`] is and for the same reason:
/// a field addition that still fits the buffer fails here, and updating
/// this constant is the prompt to check the buffer's comment.
const WORST_CASE_INFO_BODY_LEN: usize = 1389;

/// The `/api/info` body assembler in `rest.rs` writes into a fixed
/// `[u8; API_INFO_BODY_BUFFER_SIZE]` buffer via `BufferWriter`, which
/// silently discards bytes past the end. This test constructs the
/// worst-case body — every variable-width field at its maximum width —
/// and asserts that the result fits. Any future field addition that
/// pushes the total over the buffer size will fail here rather than
/// silently truncating the live response.
///
/// Worst-case assumptions (mirroring the exact `core::write!` calls in
/// `rest.rs`):
/// - `name`: 42 ASCII characters, the length of the longest current
///   `ControlBoardDescriptor::name` (`CONTROL_BOARD_ESP32S3`'s
///   "Waveshare ESP32-S3-LCD-Driver-Board W5100S").
/// - All `u64` numeric fields: `u64::MAX` (20 decimal digits).
/// - All `u32` numeric fields: `u32::MAX` (10 decimal digits).
/// - `temperature` (`i32`): `i32::MIN` (11 chars including the sign).
/// - `ambient` (`Option<i32>`): `Some(i32::MIN)` (11 chars), wider than `null`.
/// - `fan_pwm`/`pools` (`u8`/`u8`): `u8::MAX` = 255 (3 digits).
/// - `psu_fault`/`hasPreviousImage`/`resetOtaActive`: `true` (4 chars).
/// - `ip`: `"255.255.255.255"` (15 chars).
/// - `mac`: `"ff:ff:ff:ff:ff:ff"` (17 chars).
/// - `time`: `"23:59:59 UTC"` (12 chars, the fixed `format_utc_time` width).
/// - `phase`: `"rebooting"` (9 chars, longest variant).
/// - `resetReason`: `"watchdogTimeout"` (15 chars, longest variant).
/// - `resetWritePhase`: `"flashDone"` (9 chars, longest variant).
/// - `previousVersion`: `Some(u32::MAX)` (10 digits, longer than `null`).
/// - `runningBuild`: `Some` of `crate::ota::MAX_BUILD_ID_LEN` double
///   quotes, the most the registration keeps of a build id with every
///   character escaped by `write_json_str` to two bytes. The quote is
///   the widest character a build id can carry, for the reason
///   `write_json_str` gives.
/// - `running`: `true` (4 chars).
/// - `paused`: `true` (4 chars).
/// - `pause_reason`: `"temperatureUnreadable"` (21 chars, longest variant).
/// - `chains`: `ubos_metrics::chain::MAX_CHAINS` entries — the most any board
///   can report, since [`ubos_metrics::chain::ChainStatsHandle::chain_count`]
///   never exceeds the table it wraps — each with `chips` at `u16::MAX`
///   (5 digits), `frequency_mhz` at `u32::MAX` (10 digits), `hashrate`
///   at `u64::MAX` (20 digits), and `hottest_c` at `i32::MIN / 1000` (7
///   digits including the sign), the widest `state` string
///   (`"unknown"`/`"faulted"`, 7 chars).
#[test]
#[allow(clippy::too_many_lines)]
fn api_info_body_fits_in_buffer() {
    // Build the body using the same `format!` calls that `rest.rs` uses,
    // with every variable-width field set to its maximum-width value.
    let board_name = "A".repeat(42);
    let hashrate = u64::MAX;
    let uptime_secs = u64::MAX;
    let temperature = i32::MIN;
    let ambient = i32::MIN;
    let fan_rpm = u32::MAX;
    let fan_pwm = u8::MAX;
    let power_in_mw = u32::MAX;
    let power_out_mw = u32::MAX;
    let psu_voltage_set_mv = u32::MAX;
    let psu_voltage_measured_mv = u32::MAX;
    let chain_voltage_mv = u32::MAX;
    let psu_fault = true;
    let pools = u8::MAX;
    let net_confirm_seconds = u64::MAX;
    let running = true;
    let paused = true;
    let pause_reason = "temperatureUnreadable";
    let chip_reading_stale = false;

    let mut body = std::format!(
        "{{\"name\":\"{board_name}\",\
         \"hashrate\":{hashrate},\
         \"uptime\":{uptime_secs},\
         \"temperature\":{temperature},\
         \"ambient\":{ambient},\
         \"chip_reading_stale\":{chip_reading_stale},\
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
         \"pause_reason\":\"{pause_reason}\",\
         \"ip\":\"255.255.255.255\",\
         \"mac\":\"ff:ff:ff:ff:ff:ff\",\
         \"time\":\"23:59:59 UTC\""
    );

    // Chains array: the handler emits one entry per
    // `ChainStatsHandle::chain_count`, which never exceeds
    // `ubos_metrics::chain::MAX_CHAINS`, so that bound is the worst case
    // regardless of how many chains any one board actually owns.
    let chain_chips = u16::MAX;
    let chain_frequency_mhz = u32::MAX;
    let chain_hashrate = u64::MAX;
    let chain_hottest_c = i32::MIN / 1000;
    let mut chains = std::string::String::from(",\"chains\":[");
    for chain in 0..ubos_metrics::chain::MAX_CHAINS {
        if chain > 0 {
            chains.push(',');
        }
        let _ = write!(
            chains,
            "{{\"chain\":{chain},\
             \"state\":\"unknown\",\
             \"chips\":{chain_chips},\
             \"frequency_mhz\":{chain_frequency_mhz},\
             \"hashrate\":{chain_hashrate},\
             \"hottest_c\":{chain_hottest_c}}}"
        );
    }
    chains.push(']');
    body.push_str(&chains);

    // OTA block with all fields populated.
    let running_slot = "ACTIVE";
    let running_version = u32::MAX;
    // The escaped form the handler's `write_json_str` emits for a build
    // id of MAX_BUILD_ID_LEN double quotes; see the worst-case list above.
    let running_build = "\\\"".repeat(crate::ota::MAX_BUILD_ID_LEN);
    let has_previous_image = true;
    let previous_version = u32::MAX;
    let phase_str = "rebooting";
    let bytes_written = u32::MAX;
    let chunks = u32::MAX;
    let last_error = u8::MAX;
    let reset_reason_str = "watchdogTimeout";
    let reset_ota_active = true;
    let reset_ota_offset = u32::MAX;
    let reset_write_phase_str = "flashDone";

    let _ = write!(
        body,
        ",\"ota\":{{\"runningSlot\":\"{running_slot}\",\
         \"runningVersion\":{running_version},\
         \"runningBuild\":\"{running_build}\",\
         \"hasPreviousImage\":{has_previous_image},\
         \"previousVersion\":{previous_version}"
    );
    let _ = write!(
        body,
        ",\"phase\":\"{phase_str}\",\
         \"bytesWritten\":{bytes_written},\
         \"chunks\":{chunks},\
         \"lastError\":{last_error}"
    );
    let _ = write!(
        body,
        ",\"resetReason\":\"{reset_reason_str}\",\
         \"resetOtaActive\":{reset_ota_active},\
         \"resetOtaOffset\":{reset_ota_offset},\
         \"resetWritePhase\":\"{reset_write_phase_str}\"}}"
    );

    body.push('}');

    assert_eq!(
        body.len(),
        WORST_CASE_INFO_BODY_LEN,
        "worst-case /api/info body moved; update WORST_CASE_INFO_BODY_LEN"
    );
    assert!(
        body.len() < API_INFO_BODY_BUFFER_SIZE,
        "worst-case /api/info body is {} bytes, which overflows API_INFO_BODY_BUFFER_SIZE ({})",
        body.len(),
        API_INFO_BODY_BUFFER_SIZE,
    );
}

// --- CONFIG_RESPONSE_BUFFER_SIZE covers the worst-case body ---

/// Build a `heapless::String` filled to its capacity with ASCII bytes.
///
/// The capacity comes from the field the value is assigned to rather than
/// from a literal here, so widening a string in `ubos-config` widens this
/// body too instead of leaving the test measuring the old one.
fn filled<const N: usize>() -> heapless::String<N> {
    let mut string = heapless::String::new();
    for _ in 0..N {
        string.push('a').expect("BUG: fill exceeds capacity");
    }
    string
}

/// Widest decimal form `serde-json-core` emits for an `f32`, at 16 bytes.
///
/// The serializer formats floats with ryu, which switches from scientific to
/// fixed notation for small exponents, so the worst case is a long fixed form
/// rather than either extreme of the range: `f32::MIN` costs only 13 bytes.
/// This value is the widest over every `f32` bit pattern, found by
/// serializing all of them.
const WIDEST_F32: f32 = -0.000_001_000_000_1;

/// Measured length of the worst-case body.
///
/// Pinned exactly rather than merely bounded by `CONFIG_RESPONSE_BUFFER_SIZE`,
/// so the figure quoted in that constant's comment cannot drift: a field
/// addition that still fits the buffer fails here, and updating this constant
/// is the prompt to update that comment.
const WORST_CASE_BODY_LEN: usize = 2098;

/// The `GET /api/config` handler serializes a `ConfigUpdate` into a fixed
/// `[u8; CONFIG_RESPONSE_BUFFER_SIZE]`. `serde_json_core::to_slice` returns
/// `Err` rather than truncating, so an undersized buffer turns the endpoint
/// into a 500 for exactly the operators whose config is largest. This test
/// serializes the worst-case snapshot, pins its length at
/// [`WORST_CASE_BODY_LEN`] and asserts it fits, so a future field addition
/// fails here instead of in the field.
///
/// Every container is filled to the capacity its own type declares rather
/// than to a width restated here, so raising a string capacity or a slot
/// count in `ubos-config` widens this body and trips the length assert
/// instead of leaving the test measuring the narrower old one.
///
/// Worst-case assumptions:
/// - the pool list full, each slot with host and user at capacity, `u16::MAX`
///   port, `u32::MAX` weight and the wider of the two `ProtocolKind` variants,
///   which is SV2 carrying a key rather than SV1 carrying a password: a
///   `[u8; 32]` serializes as a JSON number array costing up to four bytes an
///   element, which outruns the password string;
/// - every `f32` at [`WIDEST_F32`], the widest decimal form the serializer
///   emits;
/// - every `u32` at `u32::MAX` and every `u8` at `u8::MAX`;
/// - `miner.tuning` as `Power`, the wider of the two `Tuning` variants;
/// - `api.username` and `api.password` at capacity, which is wider than the
///   live body: `ConfigUpdate::from_config` blanks the password so the stored
///   credential never reaches the client;
/// - `network` in static mode, the widest of the two `NetworkMode` variants,
///   with the DNS list full and the hostname at capacity.
#[test]
fn config_response_body_fits_in_buffer() {
    // The total below is a worst case only if the f32 fields really sit at
    // the serializer's widest form, so pin that separately rather than leave
    // it to the comment.
    let mut probe = [0u8; 32];
    let widest = serde_json_core::to_slice(&WIDEST_F32, &mut probe).expect("an f32 fits the probe");
    assert_eq!(
        widest, 16,
        "WIDEST_F32 no longer serializes to the widest f32 form"
    );

    // Which `ProtocolKind` variant is the wider one is likewise a premise of
    // the total, so pin it rather than leave it to the comment: the two
    // maxima are close enough that a capacity change could invert them.
    let widest_protocol = ubos_config::ProtocolKind::Sv2 {
        pool_pubkey: Some([u8::MAX; 32]),
    };
    let mut probe = [0u8; 256];
    let sv2 =
        serde_json_core::to_slice(&widest_protocol, &mut probe).expect("a protocol fits the probe");
    let sv1 = serde_json_core::to_slice(
        &ubos_config::ProtocolKind::Sv1 {
            password: Some(filled()),
        },
        &mut probe,
    )
    .expect("a protocol fits the probe");
    assert!(
        sv2 > sv1,
        "SV1 now serializes at least as wide as SV2 ({sv1} vs {sv2}); the \
         worst-case body no longer uses the widest protocol variant"
    );

    let pool_entry = ubos_config::PoolEntry {
        host: filled(),
        port: u16::MAX,
        user: filled(),
        protocol: widest_protocol,
        weight: u32::MAX,
    };
    let mut pool = heapless::Vec::new();
    for _ in 0..pool.capacity() {
        pool.push(pool_entry.clone())
            .expect("BUG: pool exceeds capacity");
    }

    let mut dns = heapless::Vec::new();
    for _ in 0..dns.capacity() {
        dns.push([255, 255, 255, 255])
            .expect("BUG: dns exceeds capacity");
    }

    let update = ubos_config::ConfigUpdate {
        pool: Some(pool),
        thermal: Some(ubos_config::ThermalConfig {
            sample_interval_ms: u32::MAX,
            dangerous_temp_c: WIDEST_F32,
            dangerous_chip_temp_c: WIDEST_F32,
            cooldown_temp_c: WIDEST_F32,
        }),
        cooling: Some(ubos_config::CoolingUpdate {
            mode: Some(ubos_config::FanMode::Fixed),
            target_temp_c: Some(WIDEST_F32),
            fixed_fan_pwm: Some(u8::MAX),
            min_fan_pwm: Some(u8::MAX),
        }),
        miner: Some(ubos_config::MinerConfig {
            tuning: ubos_config::Tuning::Power(ubos_config::PowerTarget {
                input_power_mw: u32::MAX,
                efficiency_mj_per_th: core::num::NonZeroU32::MAX,
            }),
        }),
        api: Some(ubos_config::ApiConfig {
            username: filled(),
            password: filled(),
        }),
        network: Some(ubos_config::NetworkConfig {
            mode: ubos_config::NetworkMode::Static {
                address: [255, 255, 255, 255],
                prefix_len: u8::MAX,
                gateway: [255, 255, 255, 255],
                dns,
            },
            hostname: filled(),
        }),
    };

    let mut buffer = std::vec![0u8; CONFIG_RESPONSE_BUFFER_SIZE * 2];
    let len = serde_json_core::to_slice(&update, &mut buffer).expect("worst case fits the probe");
    assert_eq!(
        len, WORST_CASE_BODY_LEN,
        "worst-case GET /api/config body changed; update WORST_CASE_BODY_LEN \
         and the figure quoted at CONFIG_RESPONSE_BUFFER_SIZE"
    );
    assert!(
        len <= CONFIG_RESPONSE_BUFFER_SIZE,
        "worst-case GET /api/config body is {len} bytes, which overflows \
         CONFIG_RESPONSE_BUFFER_SIZE ({CONFIG_RESPONSE_BUFFER_SIZE})"
    );
}

// --- write_chains renders the real chains array ---

/// Drives [`super::write_chains`] itself, rather than rebuilding its
/// JSON output by hand, so a typo in the handler's literal fails this
/// test instead of only ever landing in the live response.
#[test]
fn write_chains_renders_the_chains_array() {
    static CHAINS: [ubos_metrics::chain::ChainStats; 2] = [
        ubos_metrics::chain::ChainStats::new(),
        ubos_metrics::chain::ChainStats::new(),
    ];
    CHAINS[0].set_presence(ubos_metrics::chain::ChainPresence::Present { chips: 105 });
    CHAINS[0].set_frequency(ubos_core::units::Frequency::from_mhz(650));
    CHAINS[0].set_hottest(Some(72_000));
    CHAINS[1].set_presence(ubos_metrics::chain::ChainPresence::Absent);

    let active = std::boxed::Box::leak(std::boxed::Box::new(portable_atomic::AtomicU8::new(
        u8::try_from(CHAINS.len()).expect("BUG: the test table fits a u8"),
    )));
    let handle = ubos_metrics::chain::ChainStatsHandle::new(&CHAINS, active);
    let mut buffer = [0u8; 512];
    let mut writer = ubos_http::BufferWriter::new(&mut buffer);
    super::write_chains(&mut writer, handle);
    let len = writer.len();
    let json = std::str::from_utf8(&buffer[..len]).expect("write_chains emits valid UTF-8");

    assert!(
        json.starts_with(",\"chains\":[") && json.ends_with(']'),
        "unexpected chains array framing: {json}"
    );
    assert!(
        json.contains("\"chain\":0,\"state\":\"present\",\"chips\":105,\"frequency_mhz\":650"),
        "chain 0 fields missing or wrong: {json}"
    );
    assert!(
        json.contains("\"hottest_c\":72"),
        "chain 0 hottest_c missing or wrong: {json}"
    );
    assert!(
        json.contains("\"chain\":1,\"state\":\"absent\",\"chips\":0"),
        "chain 1 fields missing or wrong: {json}"
    );
}
