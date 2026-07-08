//! Tests for REST-specific helpers: `validate_board_name` and the
//! `/api/info` body-buffer size.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

use core::fmt::Write as _;

use crate::rest::{API_INFO_BODY_BUFFER_SIZE, validate_board_name};

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
/// - `fan_pwm`/`pools` (`u8`/`u8`): `u8::MAX` = 255 (3 digits).
/// - `psu_fault`/`hasPreviousImage`/`resetOtaActive`: `true` (4 chars).
/// - `ip`: `"255.255.255.255"` (15 chars).
/// - `mac`: `"ff:ff:ff:ff:ff:ff"` (17 chars).
/// - `time`: `"23:59:59 UTC"` (12 chars, the fixed `format_utc_time` width).
/// - `phase`: `"rebooting"` (9 chars, longest variant).
/// - `resetReason`: `"watchdogTimeout"` (15 chars, longest variant).
/// - `resetWritePhase`: `"flashDone"` (9 chars, longest variant).
/// - `previousVersion`: `Some(u32::MAX)` (10 digits, longer than `null`).
/// - `running`: `true` (4 chars).
/// - `paused`: `true` (4 chars).
/// - `pause_reason`: `"dangerousTemperature"` (20 chars, longest variant).
#[test]
fn api_info_body_fits_in_buffer() {
    // Build the body using the same `format!` calls that `rest.rs` uses,
    // with every variable-width field set to its maximum-width value.
    let board_name = "A".repeat(42);
    let hashrate = u64::MAX;
    let uptime_secs = u64::MAX;
    let temperature = i32::MIN;
    let fan_rpm = u32::MAX;
    let fan_pwm = u8::MAX;
    let power_in_mw = u32::MAX;
    let power_out_mw = u32::MAX;
    let voltage_set_mv = u32::MAX;
    let voltage_measured_mv = u32::MAX;
    let psu_fault = true;
    let pools = u8::MAX;
    let net_confirm_seconds = u64::MAX;
    let running = true;
    let paused = true;
    let pause_reason = "dangerousTemperature";

    let mut body = std::format!(
        "{{\"name\":\"{board_name}\",\
         \"hashrate\":{hashrate},\
         \"uptime\":{uptime_secs},\
         \"temperature\":{temperature},\
         \"fan_rpm\":{fan_rpm},\
         \"fan_pwm\":{fan_pwm},\
         \"power_in_mw\":{power_in_mw},\
         \"power_out_mw\":{power_out_mw},\
         \"voltage_set_mv\":{voltage_set_mv},\
         \"voltage_measured_mv\":{voltage_measured_mv},\
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

    // OTA block with all fields populated.
    let running_slot = "ACTIVE";
    let running_version = u32::MAX;
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

    assert!(
        body.len() < API_INFO_BODY_BUFFER_SIZE,
        "worst-case /api/info body is {} bytes, which overflows API_INFO_BODY_BUFFER_SIZE ({})",
        body.len(),
        API_INFO_BODY_BUFFER_SIZE,
    );
}
