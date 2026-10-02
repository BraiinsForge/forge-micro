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

//! Watchdog reset-reason and OTA-breadcrumb capture.

use defmt::info;

/// Capture the watchdog reset reason and OTA breadcrumb before anything else
/// touches the watchdog registers. Call this first in `boot`, before
/// `ubos_ota::mark_booted`, because `mark_booted` performs a flash op that
/// feeds the watchdog and the reason register is read-once: a debugger warm
/// reset clears it.
pub fn capture_reset_diag() {
    use embassy_rp::pac;
    use ubos_api::{OtaWritePhase, ResetReason};

    let watchdog = pac::WATCHDOG;
    let reason_reg = watchdog.reason().read();
    let scratch0 = watchdog.scratch0().read();
    let scratch1 = watchdog.scratch1().read();
    let scratch2 = watchdog.scratch2().read();

    let reason = if reason_reg.timer() {
        ResetReason::WatchdogTimeout
    } else if reason_reg.force() {
        ResetReason::WatchdogForced
    } else {
        ResetReason::Clean
    };

    let ota_was_active = scratch0 == ubos_ota::OTA_BREADCRUMB_MAGIC;
    let ota_offset = if ota_was_active { scratch1 } else { 0 };

    let write_phase = OtaWritePhase::from_u32(scratch2);

    info!(
        "reset reason: {:?}, ota_active={}, ota_offset={}, write_phase={:?}",
        reason as u8, ota_was_active, ota_offset, write_phase as u8,
    );

    ubos_api::publish_reset_diag(reason, ota_was_active, ota_offset, write_phase);

    // Clear SCRATCH0 and SCRATCH2 so a subsequent clean reset does not
    // re-report stale OTA state from this boot cycle.
    watchdog.scratch0().write_value(0u32);
    watchdog.scratch2().write_value(0u32);
}
