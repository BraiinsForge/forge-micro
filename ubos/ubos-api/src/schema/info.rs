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

//! Response shape for `GET /api/info`.
//!
//! The dashboard polls this body every two seconds and reads it by the
//! keys `ubos-webui/web/lib/types.ts` names. The top level spells its
//! keys in snake case and the `ota` block in camel case, so only
//! [`ApiInfoOta`] renames its fields.

use heapless::{String, Vec};
use serde::Serialize;
use ubos_config::POOL_SLOTS;
use ubos_core::units::{FanPwm, FanRpm, Frequency, Hashrate, Power, Temperature, Voltage};
use ubos_mining::stats::{ChainSnapshot, MAX_CHAINS};

use crate::hw_info::HwString;
use crate::ota::{OtaPhase, OtaSessionSnapshot, OtaWritePhase, ResetDiag, ResetReason};
use crate::schema::state::{ApiChainState, ApiMiningState, ApiPauseReason};
use crate::schema::system_info::OtaInfo;

/// Full snapshot for `GET /api/info`.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// Mirrors the body the dashboard reads, whose flags are JSON booleans;
// folding them into enums would change the keys it reads.
#[allow(clippy::struct_excessive_bools)]
pub struct ApiInfo {
    /// The control board's product name.
    pub name: &'static str,
    /// Sum of every chain's smoothed hashrate in hashes per second.
    pub hashrate: Hashrate,
    /// Firmware uptime in whole seconds since boot.
    pub uptime: u64,
    /// Fan speed, or zero where no fan reports. The body carries it as
    /// integer RPM.
    pub fan_rpm: FanRpm,
    /// Fan PWM duty cycle, or zero where no fan reports. The body carries
    /// it as integer percent.
    pub fan_pwm: FanPwm,
    /// Supply input power, or zero before the first reading. The body
    /// carries it as integer milliwatts under `power_in_mw`.
    #[serde(rename = "power_in_mw")]
    pub power_in: Power,
    /// Supply output power, or zero before the first reading. The body
    /// carries it as integer milliwatts under `power_out_mw`.
    #[serde(rename = "power_out_mw")]
    pub power_out: Power,
    /// Output voltage the supply was set to. The body carries it as
    /// integer millivolts under `psu_voltage_set_mv`.
    #[serde(rename = "psu_voltage_set_mv")]
    pub psu_voltage_set: Voltage,
    /// Output voltage the supply measured. The body carries it as
    /// integer millivolts under `psu_voltage_measured_mv`.
    #[serde(rename = "psu_voltage_measured_mv")]
    pub psu_voltage_measured: Voltage,
    /// Chain voltage, or zero where the board measures none. The body
    /// carries it as integer millivolts under `chain_voltage_mv`.
    #[serde(rename = "chain_voltage_mv")]
    pub chain_voltage: Voltage,
    /// `true` when the supply reports a sensor fault.
    pub psu_fault: bool,
    /// Bitmask of live pool slots: bit N is set when pool N is alive.
    pub pools: u8,
    /// Seconds left in the network confirm window, or zero when no change
    /// is pending.
    pub net_confirm_seconds: u64,
    /// `true` while the miner is in its running state.
    pub running: bool,
    /// The miner's lifecycle state, or `null` before its first publish.
    pub mining_state: Option<ApiMiningState>,
    /// `true` while at least one pause reason is active.
    pub paused: bool,
    /// The highest-priority active pause reason, or `null` when `paused`
    /// is `false`.
    pub pause_reason: Option<ApiPauseReason>,
    /// One entry per chain the board wired up, in chain order.
    #[cfg_attr(feature = "schema", schemars(with = "&[ApiInfoChain]"))]
    pub chains: Vec<ApiInfoChain, MAX_CHAINS>,
    /// IPv4 address as a dotted quad, or empty before DHCP assigns one.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = 16)))]
    pub ip: String<16>,
    /// MAC address as six colon-separated lowercase hex pairs, or empty
    /// before the board publishes one.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = 17)))]
    pub mac: String<17>,
    /// UTC wall-clock time as `HH:MM:SS UTC`, or empty before NTP
    /// synchronizes.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = 24)))]
    pub time: String<24>,
    /// The build id the board stamped: the tip of the checkout it was
    /// built from and the build time, the same id the package file is
    /// named by.
    pub build: &'static str,
    /// The machine's product name as its maker names the model, or `null`
    /// before a board publishes one.
    #[cfg_attr(feature = "schema", schemars(with = "Option<&str>", length(max = crate::hw_info::HW_STRING_LEN)))]
    pub product_name: Option<HwString>,
    /// The hashrate the machine's maker rates it at, in hashes per second,
    /// or `null` before a board publishes one.
    pub nominal_hashrate: Option<Hashrate>,
    /// OTA slot and session state. The key is left out on a board with no
    /// OTA target, and the dashboard renders its firmware card only when
    /// the key is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ota: Option<ApiInfoOta>,
}

// The index conversion below cannot saturate while both tables fit a
// byte.
const _: () = assert!(MAX_CHAINS <= u8::MAX as usize);
const _: () = assert!(POOL_SLOTS <= u8::MAX as usize);

/// A chain or pool slot index as the byte the wire carries.
fn index_u8(index: usize) -> u8 {
    u8::try_from(index).unwrap_or(u8::MAX)
}

/// One chain's entry in the `/api/info` `chains` array.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiInfoChain {
    /// The chain's index on the board.
    pub chain: u8,
    /// The chain's most recently recorded presence.
    pub state: ApiChainState,
    /// Chips the chain enumerated.
    pub chips: u16,
    /// The chain's clock. The body carries it as integer kHz under
    /// `frequency_khz`.
    #[serde(rename = "frequency_khz")]
    pub frequency: Frequency,
    /// The chain's smoothed hashrate in hashes per second.
    pub hashrate: Hashrate,
    /// The chain's hottest die in whole degrees Celsius, or `null` before
    /// a reading lands.
    pub hottest_c: Option<i32>,
    /// The pool slot the chain mines for, or `null` when it mines for none.
    pub pool: Option<u8>,
}

impl ApiInfoChain {
    #[must_use]
    pub fn new(chain: usize, stats: &ChainSnapshot, hottest: Option<Temperature>) -> Self {
        Self {
            chain: index_u8(chain),
            state: stats.presence.into(),
            chips: stats.chips(),
            frequency: stats.frequency,
            hashrate: stats.hashrate,
            hottest_c: hottest.map(Temperature::celsius_i32),
            pool: stats.active_pool.map(index_u8),
        }
    }
}

/// The `/api/info` `ota` block: the slot state, the live upload session
/// and the reset diagnostics in one flat object.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ApiInfoOta {
    /// Label of the running firmware slot.
    pub running_slot: &'static str,
    /// Version number embedded in the running image.
    pub running_version: u32,
    /// `true` when the inactive slot holds a previous image.
    pub has_previous_image: bool,
    /// Version number of the previous image, or `null` when there is none.
    pub previous_version: Option<u32>,
    /// Largest image in bytes the board accepts for an upload.
    pub max_image_bytes: u64,
    /// Phase of the current or most recent upload.
    pub phase: OtaPhase,
    /// Bytes written in the current or most recent upload.
    pub bytes_written: u32,
    /// Chunks written in the current or most recent upload.
    pub chunks: u32,
    /// Mapped error code of the last failed upload, or zero when none.
    pub last_error: u8,
    /// Cause of the most recent reset.
    pub reset_reason: ResetReason,
    /// `true` when an upload was active at the most recent reset.
    pub reset_ota_active: bool,
    /// Byte offset the upload had reached at the most recent reset.
    pub reset_ota_offset: u32,
    /// Where in a flash write the most recent reset landed.
    pub reset_write_phase: OtaWritePhase,
}

impl ApiInfoOta {
    /// A board that published no reset diagnostics reports an unknown
    /// reset with no upload active, which is what a fresh boot records.
    pub(crate) fn new(
        info: OtaInfo,
        session: OtaSessionSnapshot,
        reset: Option<ResetDiag>,
    ) -> Self {
        let reset = reset.unwrap_or(ResetDiag {
            reason: ResetReason::Unknown,
            ota_was_active: false,
            ota_offset: 0,
            write_phase: OtaWritePhase::None,
        });
        Self {
            running_slot: info.running_slot,
            running_version: info.running_version,
            has_previous_image: info.has_previous_image,
            previous_version: info.previous_version,
            max_image_bytes: info.max_image_bytes,
            phase: session.phase,
            bytes_written: session.bytes_written,
            chunks: session.chunks,
            last_error: session.last_error,
            reset_reason: reset.reason,
            reset_ota_active: reset.ota_was_active,
            reset_ota_offset: reset.ota_offset,
            reset_write_phase: reset.write_phase,
        }
    }
}
