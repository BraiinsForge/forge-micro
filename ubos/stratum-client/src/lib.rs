//! Integration layer between `mining-protocol` adapters and the
//! `AsicSession` ASIC abstraction.
//!
//! Owns the `JobRing<2>`, `WorkRing<8>`, the work generator task,
//! and the visitor impl that drives both. Generic over
//! `MiningClient`, so V1 and V2 share the same integration code.

#![no_std]

pub mod asic_session;
pub mod job_ring;
pub mod publishers;
pub mod run;
pub mod state;
pub mod work;
pub mod work_ring;

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
