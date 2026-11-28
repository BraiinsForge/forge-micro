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

#![no_std]
//! The board-agnostic OTA engine of the ubos board firmware.
//!
//! [`OtaEngine`] implements [`ubos_api::OtaTarget`] by driving an
//! [`OtaBackend`], which each platform supplies: `ubos-rp` drives
//! `embassy-boot`'s swap-based updater and `ubos-esp32` the esp-idf OTA
//! partitions. The engine owns the shared orchestration, which is
//! write-offset tracking, breadcrumb phase ordering, and the core-1 park
//! lifecycle, so no board can diverge from it. It names no hardware
//! crate, so it builds on every target and tests on the host.

mod engine;
pub use engine::{OtaBackend, OtaEngine};

pub mod shared_flash;

/// Parses a decimal `u32` from a build-time version string at compile time.
///
/// # Panics
///
/// Panics during the build on an empty string, a non-digit byte, or on
/// overflow, so a wrong version fails the compile rather than shipping
/// silently.
#[must_use]
pub const fn parse_version(raw: &str) -> u32 {
    let bytes = raw.as_bytes();
    assert!(!bytes.is_empty(), "UBOS_FW_VERSION must not be empty");
    let mut value: u32 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let digit = bytes[index];
        assert!(digit.is_ascii_digit(), "UBOS_FW_VERSION must be decimal");
        value = value * 10 + (digit - b'0') as u32;
        index += 1;
    }
    value
}
