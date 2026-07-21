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

//! ASIC mining backend over the firmware-injected chain driver.
//!
//! Owns the chip-side serial protocol and the work-dispatch loop.
//! [`Handle`] is the public surface every other crate uses to
//! observe state and issue commands. [`Miner`] is the typed state
//! machine that firmware constructs and drives; [`MinerControl`]
//! is the trait the tuner consumes. [`tuner`] hosts the `Tuner`
//! trait and its implementations.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod consts;
mod control;
pub mod miner;
pub mod publishers;
pub mod reset;
pub mod scheduler;
pub mod serial;
pub mod tuner;

pub use control::{Handle, Message, MiningState, MiningSubscription, handle};
pub use miner::{Miner, MinerControl, MinerError, PsuErrorKind};
pub use serial::{
    core1_is_parked, core1_serial_running, release_core1_park, request_core1_park,
    signal_core1_park,
};

ubos_events::events! {
    miner_reset()                                 info  = "Miner reset",
    psu_enabled()                                 info  = "PSU enabled",
    tuner_initialised()                           info  = "Tuner initialised",
    tuner_fault()                                 error = "Tuner fault",
    chip_count_mismatch(found: u8, expected: u8)  error = "Chip count {} differs from expected {}",
    frequency_ramp_started(freq_khz: u32)         info  = "Frequency ramp started to {} kHz",
    frequency_ramp_complete(freq_khz: u32)        info  = "Frequency ramp complete at {} kHz",
}
