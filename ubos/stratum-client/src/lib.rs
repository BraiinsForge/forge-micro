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

//! Integration layer between `mining-protocol` adapters and the
//! `AsicSession` ASIC abstraction.
//!
//! Owns the `JobRing<2>`, `WorkRing<8>`, the work generator, and
//! the visitor impl that drives both. Generic over `MiningClient`,
//! so V1 and V2 share the same integration code.
//!
//! How the two rings translate between the pool's and the ASIC's
//! coordinate systems: `../../../docs/STRATUM_ARCHITECTURE.md`.

#![no_std]

// The host test binary links std for the `rusty-fork` macro.
#[cfg(test)]
extern crate std;

use embassy_time::Duration;

pub mod asic_session;
pub mod connector;
pub mod extension;
pub mod job_ring;
pub mod metrics;
pub mod run;
pub mod state;
pub mod work;
pub mod work_ring;

ubos_events::events! {
    pool_unreachable(pool: u8) warn = "Pool {} unreachable",
}

/// Delay between reconnect attempts when the TCP session drops or the
/// stratum loop returns. Same cadence for every board; adjust this
/// constant if a board needs a different one.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Delay between reconnect attempts after the pool actively rejected
/// the session. A rejection such as an unknown user or failed auth
/// does not fix itself, and each SV2 attempt burns a full noise
/// handshake — about a second of CPU on the slowest boards — so it
/// retries far less often than a dropped transport.
pub const SESSION_REJECTED_DELAY: Duration = Duration::from_secs(30);

/// TCP socket buffer size for the stratum connection. Smaller than
/// the HTTP/TLS buffers because stratum JSON-RPC messages are short.
pub const STRATUM_TCP_BUFFER_SIZE: usize = 2048;

/// TCP socket timeout for the stratum connection. Long enough to
/// survive a few missed pool pings without dropping the session.
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(120);

/// Work-generator cadence in seconds when the host rolls ntime: publish
/// and advance ntime once per second. [`NtimeRoll::Host`] hands it to
/// `run_stratum_session`.
pub const NTIME_ROLL_PERIOD_SECS: u32 = 1;

/// Who advances the header's ntime while a job is current.
///
/// A chip that searches only the nonce needs the host to hand it a new
/// header every second, so the generator republishes with ntime moved on.
/// A chip that searches ntime on its own must not: a header that overlaps
/// the one it holds makes it re-find the same solutions once the write
/// restarts its scan, so the generator publishes each job exactly once
/// and the chip's driver reports the offset the chip chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NtimeRoll {
    Host,
    Chip,
}

impl From<bool> for NtimeRoll {
    fn from(chip_rolls_ntime: bool) -> Self {
        if chip_rolls_ntime {
            Self::Chip
        } else {
            Self::Host
        }
    }
}

impl NtimeRoll {
    /// The republish period, or `None` to publish each job once.
    #[must_use]
    pub const fn period_secs(self) -> Option<u32> {
        match self {
            Self::Host => Some(NTIME_ROLL_PERIOD_SECS),
            Self::Chip => None,
        }
    }
}

pub use asic_session::{AsicSession, PoolHandle, SOLUTION_QUEUE_DEPTH};
pub use connector::{PoolConnector, SessionError, run_stratum_session};
pub use extension::{NoExtension, SessionExtension};
pub use job_ring::{JOB_RING_DEPTH, JobRing, JobSlot, JobSlotError};
pub use metrics::PoolCountersHandle;
pub use run::{NTIME_ROLL_MAX_SECS, run_stratum_client};
pub use state::StratumState;
pub use work::{MAX_EXTRANONCE_2, OutstandingWork, Solution, WorkAssignment};
pub use work_ring::{WORK_RING_DEPTH, WorkRing};
