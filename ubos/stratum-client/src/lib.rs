//! Integration layer between `mining-protocol` adapters and the
//! `AsicSession` ASIC abstraction.
//!
//! Owns the `JobRing<2>`, `WorkRing<8>`, the work generator task,
//! and the visitor impl that drives both. Generic over
//! `MiningClient`, so V1 and V2 share the same integration code.

#![no_std]

use embassy_time::Duration;

pub mod asic_session;
pub mod job_ring;
pub mod publishers;
pub mod run;
pub mod state;
pub mod work;
pub mod work_ring;

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
pub use job_ring::{JobRing, JobSlot, JOB_RING_DEPTH};
pub use run::{
    record_reconnect_attempt, run_stratum_client, NTIME_ROLL_MAX_SECS, STRATUM_RECONNECTS,
    WORK_GENERATOR_PERIOD,
};
pub use state::StratumState;
pub use state::{SHARES_ACCEPTED, SHARES_REJECTED};
pub use work::{OutstandingWork, Solution, WorkAssignment, MAX_EXTRANONCE_2};
pub use work_ring::{WorkRing, WORK_RING_DEPTH};
