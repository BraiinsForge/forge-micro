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

//! Tunables for the mining serial backends.

/// Capacity of the in-memory registry that holds per-`job_key` context
/// for matching returned `ChainSolution`s back to the originating job.
/// The active slot count is the driver's `job_slots`, capped at this
/// capacity; sized to the largest `job_slots` a supported driver
/// reports, the BM1362/BM1366 `256 / 8`.
pub const WORK_REGISTRY_SLOTS: usize = 256 / 8;

/// Boot baud rate the chip listens on out of reset. The firmware crates
/// arm the UART with this before any driver exists; once the serial
/// actor runs, every re-arm goes through `ChainDriver::boot_uart_rate`,
/// so this must agree with each supported driver's boot rate. Every
/// vendor module states its own boot-rate fact, and both are 115 200
/// today; a chip booting at a different rate would need this to become
/// per-board.
pub const INITIAL_BAUDRATE: u32 = 115_200;

/// Nominal fast baud requested after PLL programming. The chip's `0x28`
/// divider produces 3.125 Mbaud (`25 MHz / 8`) for this request; the
/// host UART is programmed to [`FAST_BAUDRATE_ACTUAL`] instead.
pub const FAST_BAUDRATE: u32 = 3_125_000;

/// Host-side UART rate the serial controller is programmed to when
/// [`FAST_BAUDRATE`] is requested. This is a host compatibility value,
/// not the chip's divider output (which is 3.125 Mbaud).
pub const FAST_BAUDRATE_ACTUAL: u32 = 3_000_000;
