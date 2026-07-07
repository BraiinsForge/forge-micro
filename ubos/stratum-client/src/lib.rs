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

use embassy_time::Duration;

pub mod asic_session;
pub mod job_ring;
pub mod publishers;
pub mod run;
pub mod state;
pub mod work;
pub mod work_ring;

ubos_events::events! {
    pool_unreachable() warn = "Pool unreachable",
}

/// Delay between reconnect attempts when the TCP session drops or the
/// stratum loop returns. Same cadence for every board; adjust this
/// constant if a board needs a different one.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// TCP socket buffer size for the stratum connection. Smaller than
/// the HTTP/TLS buffers because stratum JSON-RPC messages are short.
pub const STRATUM_TCP_BUFFER_SIZE: usize = 2048;

/// TCP socket timeout for the stratum connection. Long enough to
/// survive a few missed pool pings without dropping the session.
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(120);

pub use asic_session::{AsicSession, SOLUTION_QUEUE_DEPTH};
pub use job_ring::{JOB_RING_DEPTH, JobRing, JobSlot, JobSlotError};
pub use run::{
    NTIME_ROLL_MAX_SECS, WORK_GENERATOR_PERIOD, pool_alive_bitmap, record_reconnect_attempt,
    run_stratum_client, set_pool_alive,
};
pub use state::StratumState;
pub use state::{shares_accepted, shares_rejected};
pub use work::{MAX_EXTRANONCE_2, OutstandingWork, Solution, WorkAssignment};
pub use work_ring::{WORK_RING_DEPTH, WorkRing};
