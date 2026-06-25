//! Response shape for `GET /api/system/info`.
//!
//! Carries the data the legacy `/api/info` endpoint already exposes
//! plus an `apiVariant` discriminator so machine clients can dispatch
//! on our schema without pretending to talk to a reference-device. Fields the
//! reference-miner spec lists but our Ethernet stack cannot supply (`WiFi`
//! status, ESP-IDF version) are simply absent.

use heapless::String;
use serde::Serialize;

/// OTA-related fields embedded in the system-info response.
///
/// Present when the board supplies an [`crate::ota::OtaTarget`]
/// implementation. Absent (the enclosing `Option` is `None`) on
/// boards without a dual-partition layout.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct OtaInfo {
    /// Label of the currently-running firmware slot, e.g. `"ACTIVE"`.
    pub running_slot: &'static str,
    /// Version number embedded in the currently-running firmware image.
    pub running_version: u32,
    /// `true` when a previous image is present in the inactive slot.
    pub has_previous_image: bool,
    /// Version number of the image in the inactive slot, or `null`
    /// when no previous image exists.
    pub previous_version: Option<u32>,
}

/// Full system-info snapshot for `GET /api/system/info`.
///
/// All borrowed fields live for the duration of the response build
/// call; callers do not need to keep this struct alive after
/// serialization.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo<'response> {
    /// Schema-version discriminator; always `"ubos-0.1"`.
    pub api_variant: &'static str,
    /// Human-readable board identifier, e.g. `"rp2350-001"`.
    pub board_name: &'response str,
    /// Network hostname. Set to the operator-configured hostname from
    /// `Config.network.hostname` when one has been saved; falls back
    /// to `board_name` when the stored hostname is empty.
    pub hostname: &'response str,
    /// EWMA-smoothed mining rate in hashes per second.
    pub hashrate: u64,
    /// Firmware uptime in whole seconds since boot.
    pub uptime_seconds: u64,
    /// Board temperature in degrees Celsius as reported by
    /// `ubos_thermal::run_thermal`.
    pub temperature_celsius: i32,
    /// Fan rotational speed in RPM; zero if no fan is present.
    pub fan_speed_rpm: u32,
    /// Fan PWM duty cycle in percent; zero if no fan is present.
    pub fan_pwm_percent: u8,
    /// Bitmask of connected pool slots; bit N is set when pool N is
    /// alive.
    pub pools_bitmap: u8,
    /// IPv4 address as a dotted-quad string, or empty if DHCP has not
    /// yet assigned an address.
    pub ip: String<16>,
    /// UTC wall-clock time as an `HH:MM:SS UTC` string, or empty if NTP
    /// has not yet synchronized.
    pub time: String<24>,
    /// Number of BM1362 chips on the hashboard.
    pub asic_count: u8,
    /// ASIC chip model name.
    pub asic_model: &'static str,
    /// Default mining frequency in MHz programmed at boot.
    pub default_frequency_mhz: u32,
    /// Default ASIC core voltage in millivolts programmed at boot.
    pub default_voltage_mv: u32,
    /// Accepted shares since boot, sourced from the stratum-client
    /// session counter.
    pub shares_accepted: u64,
    /// Rejected shares since boot, sourced from the stratum-client
    /// session counter.
    pub shares_rejected: u64,
    /// Total hardware shares observed since boot.
    pub shares_total: u64,
    /// Best difficulty nonce found since boot; zero until the
    /// scoreboard ring buffer is wired.
    pub best_nonce_diff: u64,
    /// Best difficulty nonce found in the current stratum session;
    /// zero until the scoreboard ring buffer is wired.
    pub best_session_nonce_diff: u64,
    /// OTA partition information. `None` on boards without a
    /// dual-partition layout or when no `OtaTarget` was supplied.
    pub ota: Option<OtaInfo>,
    /// `true` when mining is halted because at least one pause reason is
    /// active; `false` when mining is running normally.
    pub paused: bool,
    /// The highest-priority active pause reason as a camelCase string, or
    /// `null` when `paused` is `false`. See `PauseReason::as_api_str` for
    /// the canonical string values.
    pub pause_reason: Option<&'static str>,
}
