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

//! One chain of the machine: its control surface, its presence, and
//! its accounting.
//!
//! [`ChainHandle`] is the miner's own view of a chain: the serial
//! actor, the accounting slot, the reset pin and the last enumeration
//! outcome, walked together through bring-up and teardown. A tuner
//! sees the same handle only through [`ChainControl`], which the
//! submodule [`control`] defines. [`presence`] is the pure decision
//! table the enumeration loop drives, and [`stats`] the per-chain
//! accounting the API, the display and the metrics publisher read.

use embassy_time::Duration;
use ubos_core::units::Frequency;

use crate::miner::MinerError;
use crate::reset::ResetPin;
use crate::serial::Serial;

pub mod control;
pub mod presence;
pub mod stats;

pub use control::ChainControl;
pub use presence::ChainPresence;
pub use stats::{ChainStats, ChainStatsHandle};

/// One chain's serial actor, accounting, and reset pin, bundled behind
/// the [`ChainControl`] surface a tuner drives.
///
/// The reset pin is folded in here, rather than left in a parallel
/// array on `Miner`, because the miner always has to walk a chain's
/// reset pin and its serial actor together during bring-up and
/// teardown.
pub struct ChainHandle<R: ResetPin> {
    pub(crate) serial: &'static Serial,
    pub(crate) stats: &'static ChainStats,
    pub(crate) reset: R,
    pub(crate) presence: ChainPresence,
}

/// The serial actor answers in the transport's own error types; this
/// impl is where they become the one [`MinerError`] a tuner handles.
impl<R: ResetPin> ChainControl for ChainHandle<R> {
    fn chain(&self) -> usize {
        self.serial.chain()
    }

    async fn reset(&self) -> Result<(), MinerError> {
        self.serial.reset().await.map_err(MinerError::from)
    }

    async fn reset_and_enumerate(&self) -> u8 {
        self.serial.reset_and_enumerate().await
    }

    async fn configure(&self) -> Result<(), MinerError> {
        self.serial.configure().await.map_err(MinerError::from)
    }

    async fn pvt_sweep(&self) -> Result<(), MinerError> {
        self.serial.pvt_sweep().await.map_err(MinerError::from)
    }

    /// Program the hash-clock frequency. The serial actor records it
    /// in this chain's accounting on success.
    async fn set_frequency(&self, freq: Frequency) -> Result<(), MinerError> {
        self.serial
            .set_frequency(freq)
            .await
            .map_err(MinerError::from)
    }

    async fn start_sending_work(&self, period: Duration) -> Result<(), MinerError> {
        self.serial
            .start_sending_work(period)
            .await
            .map_err(MinerError::from)
    }

    async fn stop_sending_work(&self) -> Result<(), MinerError> {
        self.serial
            .stop_sending_work()
            .await
            .map_err(MinerError::from)
    }
}
