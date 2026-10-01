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

//! Async PSU driver for the BMM101 miniminer hashboard.
//!
//! Generic over `embedded_hal_async::i2c::I2c`. Runs unchanged on
//! Linux (via `embedded-i2c-unix`) and on rp2040/rp2350 (via
//! `embassy-rp::i2c`).
//!
//! A board builds one [`PowerSupply`] over its driver and spawns
//! [`PowerSupply::run`] with the [`Runner`] that [`Handle::take`]
//! returns; every other task reaches the PSU through a copy of the
//! [`Handle`].

#![cfg_attr(not(test), no_std)]
// Embassy executors do not require `Send`; adding the bound to
// every trait method's future would force every I2C impl and all
// internal state to be `Send`, which is not free on no_std.
#![allow(async_fn_in_trait)]
// Hardware register getters are intentionally side-effect-free
// returns; #[must_use] sprinkled across every accessor is noise.
#![allow(clippy::must_use_candidate)]
// Bounded casts inside hardware drivers (DAC byte, register
// pointer) are safe by construction; the math above each cast
// rejects out-of-range values explicitly.
#![allow(clippy::cast_possible_truncation)]

ubos_events::events! {
    psu_fault() error = "PSU fault",
}

pub mod bfm;
pub mod bmm;
pub mod config;
mod error;
pub mod forge_axe;
pub mod gated;
pub mod heartbeat;
pub mod ina230;
mod pmbus;
mod psu;
pub mod sic45x;
pub mod state;
mod status;
mod supply;
pub mod tps53647;
pub mod tps6287x;

pub use bfm::{BfmDevice, BfmPsu, BfmPsuDescriptor, BfmShared};
pub use bmm::{BmmDevice, BmmPsu, BmmShared};
pub use config::BmmPsuDescriptor;
pub use error::{CommandError, Error};
pub use forge_axe::{ForgeAxeDevice, ForgeAxePsu, ForgeAxePsuDescriptor, ForgeAxeShared};
pub use gated::{GatedPsu, InnerGate};
pub use psu::{MODEL_LEN, ModelString, Psu, VoltageSource};
pub use state::{Device, RawMutex, Ready, Shared, State};
pub use status::StatusFlags;
pub use supply::{
    COMMAND_GRACE, HEARTBEAT_TIMEOUT, Handle, POWER_STATS_INTERVAL, PowerSupply, PsuReading,
    PsuSubscription, Runner,
};
