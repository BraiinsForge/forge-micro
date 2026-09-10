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

//! Subsystem-scoped constants, the hardware-watchdog feed loop, the
//! hold-to-trigger button monitor, and the I2C bus clear. The
//! "core" of the firmware framework: every other ubos-* crate depends on
//! this one for shared values and the watchdog and button task wrappers.
#![no_std]

pub mod button;
#[cfg(feature = "ccmram-bss")]
pub mod ccmram;
pub mod consts;
pub mod i2c;
pub mod reboot;
pub mod units;
pub mod watchdog;
