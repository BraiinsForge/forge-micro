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
//! machine that firmware constructs and drives. [`tuner`] holds the
//! seam the miner selects a tuner through, and the fixed and power
//! policies; the miner applies their plans.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod chain;
pub mod consts;
mod control;
pub mod miner;
mod park;
pub mod publishers;
mod registry;
pub mod reset;
pub mod scheduler;
pub mod serial;
pub mod tuner;

pub use chain::ChainHandle;
pub use control::{Handle, Message, MiningState, MiningSubscription, handle};
pub use miner::{Miner, MinerError, PsuErrorKind};
pub use park::{Park, ParkRequest};
pub use serial::{
    core1_is_parked, core1_serial_running, release_core1_park, request_core1_park,
    signal_core1_park,
};

use ubos_core::units::Frequency;

ubos_events::events! {
    miner_reset()                                 info  = "Miner reset",
    pool_available()                              info  = "A pool holds a job; starting",
    pool_lost()                                   warn  = "No pool holds a job; rail cut",
    psu_enabled()                                 info  = "PSU enabled",
    work_initialized()                            info  = "Work initialized",
    work_initialization_failed()                  error = "Work initialization failed",
    tuner_fault()                                 error = "Tuner fault",
    chain_enumerated_chips(chain: u8, found: u8, expected: u8)
                                                   info  = "Chain {} enumerated {} of {} chips",
    chain_chip_count_mismatch(chain: u8, found: u8, expected: u8)
                                                   error = "Chain {} has {} chips, expected {}",
    chain_absent(chain: u8)                       info  = "Chain {} has no hashboard",
    chains_parked(cycles: u8)                     error = "No chain answered in {} cycles. The miner is parked until Start",
    chain_configure_failed(chain: u8, attempt: u8) error = "Chain {} refused its configuration on attempt {}",
    frequency_ramp_began(frequency: Frequency)    info  = "Frequency ramp started to {} MHz",
    frequency_ramp_finished(frequency: Frequency) info  = "Frequency ramp complete at {} MHz",
}
