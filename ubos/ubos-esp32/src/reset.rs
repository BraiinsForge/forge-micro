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

//! Boot-time reset-reason capture.

use defmt::{info, warn};
use esp_hal::rtc_cntl::SocResetReason;
use ubos_api::{OtaWritePhase, ResetReason};

/// Report why the chip last reset and publish it for the OTA reset
/// diagnostics. A watchdog reason means the previous run stalled the
/// executor past `config::Watchdog::TIMEOUT`; a power-on or external reset
/// reads as None. Unlike the RP boards there is no reset-surviving scratch
/// register, so the OTA breadcrumb fields are absent: the two-slot otadata
/// scheme is self-healing, so a reset is never an in-progress OTA write.
pub fn capture_reset_diag() {
    let reset_reason = match esp_hal::system::reset_reason() {
        Some(
            SocResetReason::CoreMwdt0
            | SocResetReason::CoreMwdt1
            | SocResetReason::CpuMwdt0
            | SocResetReason::CpuMwdt1
            | SocResetReason::CoreRtcWdt
            | SocResetReason::CpuRtcWdt
            | SocResetReason::SysRtcWdt
            | SocResetReason::SysSuperWdt,
        ) => {
            warn!("last reset: watchdog");
            ResetReason::WatchdogTimeout
        }
        Some(SocResetReason::SysBrownOut) => {
            warn!("last reset: brownout");
            ResetReason::Unknown
        }
        Some(SocResetReason::CoreSw | SocResetReason::CpuSw) => {
            info!("last reset: software");
            ResetReason::Clean
        }
        Some(SocResetReason::ChipPowerOn) | None => {
            info!("last reset: power-on or external");
            ResetReason::Clean
        }
        Some(
            SocResetReason::CoreDeepSleep
            | SocResetReason::SysClkGlitch
            | SocResetReason::CoreEfuseCrc
            | SocResetReason::CoreUsbUart
            | SocResetReason::CoreUsbJtag
            | SocResetReason::CorePwrGlitch,
        ) => {
            info!("last reset: other");
            ResetReason::Unknown
        }
    };
    ubos_api::publish_reset_diag(reset_reason, false, 0, OtaWritePhase::None);
}
