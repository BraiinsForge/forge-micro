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

//! RP2040 bootloader using `embassy-boot-rp`.
//!
//! Initialises the flash peripheral via `WatchdogFlash`, reads the partition
//! offsets baked into the linker symbols from `memory.x`, and chains to the
//! ACTIVE partition. Control never returns from `load()`.
//!
//! The RP2040 requires a 256-byte second-stage bootloader (boot2) at the
//! start of flash. `embassy-rp` embeds it automatically via the
//! `select_bootloader!` macro when the `rp2040` feature is active and
//! `boot2-none` is not set; `link-rp.x` places it at address 0x10000000.
//! The RP2040 bootrom verifies its CRC32 before handing off control.
//!
//! Cortex-M0+ has no instruction cache, so `BootLoader::load` skips the
//! `SCB.invalidate_icache()` call that the RP2350 path issues; this is
//! correct and already handled by embassy-boot-rp's `#[cfg(not(armv6m))]`
//! guard on that call.

#![no_std]
#![no_main]

use cortex_m_rt::entry;

const FLASH_SIZE: usize = 2 * 1024 * 1024;

#[entry]
fn main() -> ! {
    rp_bootloader::boot::<FLASH_SIZE>()
}
