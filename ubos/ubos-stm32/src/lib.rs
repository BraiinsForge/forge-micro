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

//! Per-platform support for the STM32F407 and STM32F439 control boards.
//!
//! The crate holds the fan, miner-serial, watchdog and Ethernet device code
//! that both STM32 boards carried one copy of: the timer-based fan PWM and
//! tachometer module with its `tach_task!` macro, the `MinerUart` newtype,
//! the `BoardWatchdog` wrapper, and the `Device` type alias with its
//! `ethernet_device!` macro. A board depends on this crate and deletes its
//! own copies.
//!
//! Both boards are single-core, so there is no core-1 spawn macro here, and
//! neither declares a `#[panic_handler]` or a `HardFault` handler, so there
//! is nothing to hoist for fault handling.

#![no_std]

pub mod fan;
pub mod uart;

mod eth;
mod watchdog;

pub use eth::Device;
pub use watchdog::BoardWatchdog;
