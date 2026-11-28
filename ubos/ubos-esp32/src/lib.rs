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

//! Per-platform support for the Xtensa ESP32-S3 control board.
//!
//! The `ota` module owns the OTA flash backend; the other modules own the
//! platform boot, idle and watchdog code.

#![no_std]

pub mod cpu_load;
pub mod expander;
pub mod fan;
pub mod i2c;
pub mod ota;
pub mod uart;
pub mod watchdog;

mod core1;
mod cpu_idle;
mod idle;
mod reset;
mod ring;

pub use idle::IdleAccounting;
pub use reset::capture_reset_diag;
pub use ring::PersistentRing;
pub use watchdog::BoardWatchdog;

/// Fill a 32-byte seed from the hardware RNG, one `u32` call at a time.
#[must_use]
pub fn seed_from_rng(rng: &mut esp_hal::rng::Rng) -> [u8; 32] {
    let mut seed = [0u8; 32];
    for chunk in seed.as_chunks_mut::<4>().0 {
        *chunk = rng.random().to_le_bytes();
    }
    seed
}
