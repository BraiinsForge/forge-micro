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
use ubos_metrics::chain::{ChainPresence, ChainStats, MAX_CHAINS};

use crate::hw_info::HwString;
use crate::ota::{OtaPhase, OtaSessionSnapshot, OtaWritePhase, ResetDiag, ResetReason};
use crate::schema::system_info::OtaInfo;

/// Millidegrees in one degree Celsius, the unit `hottest_c` is reported in.
const MILLICELSIUS_PER_CELSIUS: i32 = 1000;

/// Full snapshot for `GET /api/info`.
#[derive(Serialize)]
// Mirrors the body the dashboard reads, whose flags are JSON booleans;
// folding them into enums would change the keys it reads.
#[allow(clippy::struct_excessive_bools)]
pub struct ApiInfo {
    /// The control board's product name, from
    /// `ControlBoardDescriptor::name`.
    pub name: &'static str,
    /// Sum of every chain's smoothed hashrate in hashes per second.
    pub hashrate: u64,
    /// Firmware uptime in whole seconds since boot.
    pub uptime: u64,
    /// Hottest chip temperature in whole degrees Celsius, or zero before
    /// the first measurement.
    pub temperature: i32,
    /// Ambient temperature in whole degrees Celsius, or `null` where the
    /// board measures none.
    pub ambient: Option<i32>,
    /// `true` when `temperature` is not a fresh measurement.
    pub chip_reading_stale: bool,
    /// Fan speed in RPM, or zero where no fan reports.
    pub fan_rpm: u32,
    /// Fan PWM duty cycle in percent, or zero where no fan reports.
    pub fan_pwm: u8,
    /// Supply input power in milliwatts, or zero before the first reading.
    pub power_in_mw: u32,
    /// Supply output power in milliwatts, or zero before the first reading.
    pub power_out_mw: u32,
    /// Output voltage the supply was set to, in millivolts.
    pub psu_voltage_set_mv: u32,
    /// Output voltage the supply measured, in millivolts.
    pub psu_voltage_measured_mv: u32,
    /// Chain voltage in millivolts, or zero where the board measures none.
    pub chain_voltage_mv: u32,
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
    /// `MiningState::as_api_str` gives the strings.
    pub mining_state: Option<&'static str>,
    /// `true` while at least one pause reason is active.
    pub paused: bool,
    /// The highest-priority active pause reason, or `null` when `paused`
    /// is `false`. `PauseReason::as_api_str` gives the strings.
    pub pause_reason: Option<&'static str>,
    /// One entry per chain the board wired up, in chain order.
    pub chains: Vec<ApiInfoChain, MAX_CHAINS>,
    /// IPv4 address as a dotted quad, or empty before DHCP assigns one.
    pub ip: String<16>,
    /// MAC address as six colon-separated lowercase hex pairs, or empty
    /// before the board publishes one.
    pub mac: String<17>,
    /// UTC wall-clock time as `HH:MM:SS UTC`, or empty before NTP
    /// synchronizes.
    pub time: String<24>,
    /// The build id the board stamped, cut to
    /// [`crate::ota::MAX_BUILD_ID_LEN`] bytes.
    pub build: &'static str,
    /// The machine's product name as its maker names the model, or `null`
    /// before a board publishes one.
    pub product_name: Option<HwString>,
    /// OTA slot and session state. The key is left out on a board with no
    /// OTA target, and the dashboard renders its firmware card only when
    /// the key is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ota: Option<ApiInfoOta>,
}

/// One chain's entry in the `/api/info` `chains` array.
#[derive(Serialize)]
pub struct ApiInfoChain {
    /// The chain's index on the board.
    pub chain: usize,
    /// `unknown`, `present`, `absent` or `faulted`, from the chain's most
    /// recently recorded presence.
    pub state: &'static str,
    /// Chips the chain enumerated.
    pub chips: u16,
    /// The chain's clock in kHz.
    pub frequency_khz: u32,
    /// The chain's smoothed hashrate in hashes per second.
    pub hashrate: u64,
    /// The chain's hottest die in whole degrees Celsius, or `null` before
    /// a reading lands.
    pub hottest_c: Option<i32>,
    /// The pool slot the chain mines for, or `null` when it mines for none.
    pub pool: Option<usize>,
}

impl ApiInfoChain {
    /// The entry for chain `chain`, read from its accounting.
    #[must_use]
    pub fn new(chain: usize, stats: &ChainStats) -> Self {
        Self {
            chain,
            state: match stats.presence() {
                ChainPresence::Unknown => "unknown",
                ChainPresence::Present { .. } => "present",
                ChainPresence::Absent => "absent",
                ChainPresence::Faulted { .. } => "faulted",
            },
            chips: stats.chips(),
            frequency_khz: stats.frequency().khz(),
            hashrate: stats.hashrate(),
            hottest_c: stats
                .hottest_mc()
                .map(|hottest_mc| hottest_mc / MILLICELSIUS_PER_CELSIUS),
            pool: stats.active_pool(),
        }
    }
}

/// The `/api/info` `ota` block: the slot state, the live upload session
/// and the reset diagnostics in one flat object.
#[derive(Serialize)]
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
    /// The block for a board with an OTA target. A board that has not
    /// published its reset diagnostics reports an unknown reset with no
    /// upload active, which is what a fresh boot records.
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
