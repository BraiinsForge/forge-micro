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

//! Per-platform support for the RP2040 and RP2350 control boards.
//!
//! The `#[panic_handler]` and the `HardFault` exception are library symbols
//! here, so a board must name the crate for the linker to take them from it.
//!
//! `ubos-ota/rp` owns the OTA flash backend and the breadcrumb API; this
//! crate owns the platform boot, fault and watchdog code.

#![no_std]

pub mod boot;
pub mod fan;
pub mod i2c;
pub mod pio_tach;
pub mod rgb;
pub mod uart;

mod core1;
mod fault;
mod reset;
mod watchdog;

pub use reset::capture_reset_diag;
pub use watchdog::BoardWatchdog;

/// Software ring buffer size per direction for the core-1 `BufferedUart`. The
/// RP hardware FIFO is 32 bytes; 1 KiB gives slack for ETH-ISR jitter at the
/// chip-serial baud rate.
pub const UART_RING_BUFFER_SIZE: usize = 1024;
