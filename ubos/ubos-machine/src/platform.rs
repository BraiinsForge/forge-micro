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

//! Control-board MCU platforms.

/// The MCU a control board is built around.
///
/// Identity and reporting metadata only. The concrete HAL, target, and
/// memory layout are selected by the firmware binary crate, not by this
/// data, because they are compile-time choices the type system fixes.
#[derive(Clone, Copy)]
pub struct PlatformDescriptor {
    /// Short platform name. Matches the firmware crate directory for
    /// every platform except `hashnode`, which reuses the `RP2350`
    /// platform const and so reports `"rp2350"`.
    pub name: &'static str,
    pub core: &'static str,
    pub target: &'static str,
}

pub const RP2040: PlatformDescriptor = PlatformDescriptor {
    name: "rp2040",
    core: "cortex-m0plus",
    target: "thumbv6m-none-eabi",
};

pub const RP2350: PlatformDescriptor = PlatformDescriptor {
    name: "rp2350",
    core: "cortex-m33",
    target: "thumbv8m.main-none-eabihf",
};

pub const ESP32S3: PlatformDescriptor = PlatformDescriptor {
    name: "esp32s3",
    core: "xtensa-lx7",
    target: "xtensa-esp32s3-none-elf",
};
