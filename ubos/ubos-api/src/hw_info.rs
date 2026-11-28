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
//! hashboard EEPROM read publishes its parsed serials the same way, and
//! a board whose stored identity names the machine and its brand
//! publishes that product name and brand name. A board that knows the
//! hashrate its maker rates the machine at publishes that nominal
//! hashrate. Boards that never publish a given cell leave its reader
//! returning `None`.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use heapless::{String, Vec};
use ubos_core::units::Hashrate;

#[cfg(test)]
#[path = "tests/hw_info.rs"]
mod tests;

/// Capacity of every string field, sized to the largest producer, a
/// hashboard EEPROM serial.
pub const HW_STRING_LEN: usize = 40;

/// An owned hardware-identity string.
pub type HwString = String<HW_STRING_LEN>;

static BOOTLOADER_VERSION: Watch<CriticalSectionRawMutex, HwString, 1> = Watch::new();
static FAN_CONTROLLER_VERSION: Watch<CriticalSectionRawMutex, HwString, 1> = Watch::new();
static PRODUCT_NAME: Watch<CriticalSectionRawMutex, HwString, 1> = Watch::new();
static BRAND_NAME: Watch<CriticalSectionRawMutex, HwString, 1> = Watch::new();
static NOMINAL_HASHRATE: Watch<CriticalSectionRawMutex, Hashrate, 1> = Watch::new();

/// Per-slot hashboard serials, indexed by physical chain. A board with
/// fewer chains leaves the tail empty.
pub type HashboardIds = Vec<Option<HwString>, { ubos_mining::stats::MAX_CHAINS }>;

static HASHBOARD_IDS: Watch<CriticalSectionRawMutex, HashboardIds, 1> = Watch::new();

/// Publishes the bootloader version string. Call once at boot.
pub fn publish_bootloader_version(version: HwString) {
    BOOTLOADER_VERSION.sender().send(version);
}

pub(crate) fn bootloader_version() -> Option<HwString> {
    BOOTLOADER_VERSION.try_get()
}

/// Publishes the fan controller version string. Call once at boot.
pub fn publish_fan_controller_version(version: HwString) {
    FAN_CONTROLLER_VERSION.sender().send(version);
}

pub(crate) fn fan_controller_version() -> Option<HwString> {
    FAN_CONTROLLER_VERSION.try_get()
}

/// Publishes the machine's product name, as its maker names the model
/// on the machine. Called once at boot.
pub fn publish_product_name(name: HwString) {
    PRODUCT_NAME.sender().send(name);
}

pub(crate) fn product_name() -> Option<HwString> {
    PRODUCT_NAME.try_get()
}

/// Publishes the brand the machine ships under. Call once at boot.
pub fn publish_brand_name(name: HwString) {
    BRAND_NAME.sender().send(name);
}

/// Returns the published brand name, or `None` before publication and
/// on boards that never publish one.
pub(crate) fn brand_name() -> Option<HwString> {
    BRAND_NAME.try_get()
}

/// Publishes the hashrate the machine's maker rates it at, so an
/// operator can hold what the machine mines against its sticker. Called
/// once at boot.
pub fn publish_nominal_hashrate(nominal: Hashrate) {
    NOMINAL_HASHRATE.sender().send(nominal);
}

pub(crate) fn nominal_hashrate() -> Option<Hashrate> {
    NOMINAL_HASHRATE.try_get()
}

/// Publishes the per-chain hashboard serials. Called once at boot.
pub fn publish_hashboard_ids(ids: HashboardIds) {
    HASHBOARD_IDS.sender().send(ids);
}

/// Publishes a single-chain board's serial in slot 0; a multi-chain
/// board calls [`publish_hashboard_ids`].
pub fn publish_hashboard_id(serial: Option<HwString>) {
    const _: () = assert!(
        ubos_mining::stats::MAX_CHAINS >= 1,
        "one slot must fit the one serial"
    );
    let mut ids = HashboardIds::new();
    let _ = ids.push(serial);
    publish_hashboard_ids(ids);
}

pub(crate) fn hashboard_ids() -> Option<HashboardIds> {
    HASHBOARD_IDS.try_get()
}
