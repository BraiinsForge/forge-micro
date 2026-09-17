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

//! The chain-facing control surface the miner hands a tuner.
//!
//! This is the object that stands for one chain to the tuner:
//! [`crate::ChainHandle`] implements it over a real chain's serial
//! actor, accounting and reset pin, and a test can implement it against
//! a mock instead. The serial actor itself does not: it is one end of
//! the machine, not a whole chain, and its own methods speak the
//! transport's errors. Every method here reports [`MinerError`] instead,
//! so a tuner handles one error type and never takes on `embedded_io`
//! or `ubos_asic` error vocabulary.
//!
//! Bring-up is not on this surface. Reset, enumeration and the
//! safe-boot configuration are the miner's, and a chain reaches a
//! tuner only after they are done: configured, on the fast UART, with
//! one temperature reading taken. What is left is the ramp.

use embassy_time::Duration;
use ubos_core::units::Frequency;

use crate::miner::MinerError;

#[allow(async_fn_in_trait)]
pub trait ChainControl {
    /// This chain's index among the machine's hashboards.
    fn chain(&self) -> usize;

    /// Program the hash-clock frequency.
    async fn set_frequency(&self, freq: Frequency) -> Result<(), MinerError>;

    /// Begin dispatching work at `period`.
    async fn start_sending_work(&self, period: Duration) -> Result<(), MinerError>;

    /// Stop dispatching work.
    async fn stop_sending_work(&self) -> Result<(), MinerError>;
}
