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

//! Per-chain hardware commands used by the miner to execute tuner plans.
//!
//! [`crate::ChainHandle`] implements this surface over the serial actor.
//! Commands report [`MinerError`]; the synchronous policy receives readings
//! and returns plans without access to hardware. Bring-up stays on the handle.

use embassy_time::Duration;
use ubos_core::units::Frequency;

use crate::miner::MinerError;

#[allow(async_fn_in_trait)]
pub(crate) trait ChainControl {
    /// This chain's index among the machine's hashboards.
    fn chain(&self) -> usize;

    /// Program the hash-clock frequency.
    async fn set_frequency(&self, freq: Frequency) -> Result<(), MinerError>;

    /// Begin dispatching work at `period`.
    async fn start_sending_work(&self, period: Duration) -> Result<(), MinerError>;
}
