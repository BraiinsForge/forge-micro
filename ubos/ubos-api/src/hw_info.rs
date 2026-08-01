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

//! Boot-time-captured hardware identity published by board code.
//!
//! A board with a second-stage bootloader and a fan-controller
//! co-processor reads their version strings once, early in boot, and
//! publishes them here through a `Watch` so the system-info handler can
//! read the latest snapshot lock-free. Every board's boot-time
//! hashboard EEPROM read publishes its parsed serials the same way.
//! Boards that never publish a given cell leave its reader returning
//! `None`.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use heapless::{String, Vec};

/// Capacity of every string field in the hardware-identity response.
/// Sized to the largest producer, a 40-byte hashboard EEPROM serial, so
/// no board's data needs truncating and no field encodes a
/// producer-specific width.
pub const HW_STRING_LEN: usize = 40;

/// An owned hardware-identity string; all `hwInfo` string fields and
/// publish cells share this one type.
pub type HwString = String<HW_STRING_LEN>;

static BOOTLOADER_VERSION: Watch<CriticalSectionRawMutex, HwString, 1> = Watch::new();
static FAN_CONTROLLER_VERSION: Watch<CriticalSectionRawMutex, HwString, 1> = Watch::new();

/// Capacity of the per-slot serial vector below, sized for the largest
/// chain count this API has to describe rather than for any board in
/// this tree, all of which drive a single hashboard. A machine with
/// more chains raises it; a board with fewer leaves the tail empty.
pub const MAX_HASHBOARDS: usize = 3;

/// Per-slot hashboard serials; the index is the physical chain number.
pub type HashboardIds = Vec<Option<HwString>, MAX_HASHBOARDS>;

static HASHBOARD_IDS: Watch<CriticalSectionRawMutex, HashboardIds, 1> = Watch::new();

/// Publishes the bootloader version string. Call once at boot.
pub fn publish_bootloader_version(version: HwString) {
    BOOTLOADER_VERSION.sender().send(version);
}

/// Returns the published bootloader version, or `None` before
/// publication and on boards that never publish one.
pub(crate) fn bootloader_version() -> Option<HwString> {
    BOOTLOADER_VERSION.try_get()
}

/// Publishes the fan controller version string. Call once at boot.
pub fn publish_fan_controller_version(version: HwString) {
    FAN_CONTROLLER_VERSION.sender().send(version);
}

/// Returns the published fan controller version, or `None` before
/// publication and on boards that never publish one.
pub(crate) fn fan_controller_version() -> Option<HwString> {
    FAN_CONTROLLER_VERSION.try_get()
}

/// Publishes the per-chain hashboard serials. Call once at boot, after
/// the boot-time EEPROM read(s) complete.
pub fn publish_hashboard_ids(ids: HashboardIds) {
    HASHBOARD_IDS.sender().send(ids);
}

/// Returns the published hashboard serials, or `None` before
/// publication and on boards that never publish any.
pub(crate) fn hashboard_ids() -> Option<HashboardIds> {
    HASHBOARD_IDS.try_get()
}

#[cfg(test)]
#[path = "tests/hw_info.rs"]
mod tests;
