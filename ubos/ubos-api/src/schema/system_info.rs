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

//! Response shape for `GET /api/system/info`.
//!
//! Carries the data the legacy `/api/info` endpoint already exposes
//! plus an `apiVariant` discriminator so machine clients can dispatch
//! on our schema without pretending to talk to a reference-device. Fields the
//! reference-miner spec lists but our Ethernet stack cannot supply (`WiFi`
//! status, ESP-IDF version) are simply absent.

use heapless::String;
use serde::Serialize;

use crate::hw_info::{HashboardIds, HwString};

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
    /// Maximum firmware image size in bytes this board accepts for an OTA
    /// upload, so a client can cap its file picker at the real slot size.
    pub max_image_bytes: u64,
}

/// Hardware-identification fields embedded in the system-info response.
///
/// `psu_model` is detected at runtime, at the PSU driver's first
/// successful init, and reads `null` where the supply is not
/// identifiable.
/// `bootloader_version` and `fan_controller_version` are read once at
/// boot, on the boards that have both, and reach this struct through
/// the publish/read cells in `crate::hw_info`; they are `null` before
/// publication and on every board that publishes neither.
/// `hashboard_ids` is published by every board's boot-time hashboard
/// EEPROM read the same way, `product_name` by a board whose stored
/// identity names the machine, and `nominal_hashrate` by a board that
/// knows the hashrate its maker rates the machine at.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HwInfo {
    /// The machine's product name as its maker names the model, or
    /// `null` before the boot-time read publishes it and on boards that
    /// publish none.
    pub product_name: Option<HwString>,
    /// The hashrate the machine's maker rates it at, in hashes per
    /// second, or `null` before the boot-time read publishes it and on
    /// boards that publish none.
    pub nominal_hashrate: Option<u64>,
    /// Bootloader CRC-32 fingerprint as 8-char lowercase hex, or `null`
    /// before the boot-time capture runs or on boards that do not
    /// publish one.
    pub bootloader_version: Option<HwString>,
    /// Fan-controller co-processor version string, or `null`
    /// before the boot-time capture runs or on boards that do not
    /// publish one.
    pub fan_controller_version: Option<HwString>,
    /// PSU model name, detected at the PSU driver's first successful
    /// init. A driver whose protocol exposes one reports the part
    /// together with its revision letter. `null` where the supply is
    /// not identifiable.
    pub psu_model: Option<HwString>,
    /// Per-chain hashboard serials, index-positional: element N is the
    /// serial read off physical chain N. An element is `null` when that
    /// chain's boot-time probe or read failed — an absent board and an
    /// unreadable one both report `null`, distinct from a successful
    /// read of a corrupt record, which still returns `"INVALID"`. The
    /// whole field is `null` until the boot-time read publishes, and
    /// stays `null` forever on a board with no EEPROM producer wired up.
    pub hashboard_ids: Option<HashboardIds>,
    /// Count of chains whose most recently recorded presence is
    /// `Present`, from
    /// `ubos_metrics::chain::ChainStatsHandle::present_chain_count`.
    pub chains_present: u8,
}

/// Full system-info snapshot for `GET /api/system/info`.
///
/// All borrowed fields live for the duration of the response build
/// call; callers do not need to keep this struct alive after
/// serialization.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo<'response> {
    /// Schema-version discriminator from [`super::API_VARIANT`].
    pub api_variant: &'static str,
    /// The control board's product name, reported verbatim from
    /// `ControlBoardDescriptor::name`, e.g. `"W6100-EVB-Pico2"`.
    pub board_name: &'response str,
    /// The build id the board stamped: the tip of the checkout it was built
    /// from and the build time, the same id the package file is named by.
    /// Cut to [`crate::ota::MAX_BUILD_ID_LEN`] bytes, which the body
    /// buffers are sized for.
    pub build: &'response str,
    /// Network hostname. Set to the operator-configured hostname from
    /// `Config.network.hostname` when one has been saved; falls back
    /// to `board_name` when the stored hostname is empty.
    pub hostname: &'response str,
    /// EWMA-smoothed mining rate in hashes per second.
    pub hashrate: u64,
    /// Firmware uptime in whole seconds since boot.
    pub uptime_seconds: u64,
    /// Ambient temperature around the board in degrees Celsius, on a
    /// board that measures it. `None` where the board has no sensor for
    /// it. What the reading is for is `AmbientSensor`'s doc in
    /// `ubos-thermal` to say.
    pub ambient_celsius: Option<i32>,
    /// `true` when the board has no fresh temperature measurement: either
    /// no sensor has answered a read yet, or the sensors went silent. See
    /// "Two ways out of an overheat halt" in `docs/MINING.md` for why a
    /// board whose chips are its thermometer can go silent for a while.
    pub chip_reading_stale: bool,
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
    /// Default mining frequency in kHz programmed at boot.
    pub default_frequency_khz: u32,
    /// Default ASIC core voltage in millivolts programmed at boot.
    pub default_voltage_mv: u32,
    /// Accepted shares since boot, sourced from the stratum-client
    /// session counter.
    pub shares_accepted: u64,
    /// Rejected shares since boot, sourced from the stratum-client
    /// session counter.
    pub shares_rejected: u64,
    /// Total chip shares since boot, in units of 2^32 expected
    /// hashes: a solution at chip difficulty `d` counts `d`.
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
    /// The miner's lifecycle state as a camelCase string, or `null`
    /// before its first publish. See `MiningState::as_api_str` for the
    /// canonical string values.
    pub mining_state: Option<&'static str>,
    /// `true` when mining is halted because at least one pause reason is
    /// active; `false` when mining is running normally.
    pub paused: bool,
    /// The highest-priority active pause reason as a camelCase string, or
    /// `null` when `paused` is `false`. See `PauseReason::as_api_str` for
    /// the canonical string values.
    pub pause_reason: Option<&'static str>,
    /// Hardware-identification fields: PSU model and the
    /// boot-time-captured version strings a board may publish.
    pub hw_info: HwInfo,
}
