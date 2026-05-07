//! Integration layer between `mining-protocol` adapters and the
//! `AsicSession` ASIC abstraction.
//!
//! Owns the `JobRing<2>`, `WorkRing<8>`, the work generator task,
//! and the visitor impl that drives both. Generic over
//! `MiningClient`, so V1 and V2 share the same integration code.
//! Replaces `ubos-backend`.

#![no_std]

pub mod asic_session;
pub mod job_ring;
pub mod state;
pub mod work;
pub mod work_ring;

pub use asic_session::{AsicSession, SOLUTION_QUEUE_DEPTH};
pub use job_ring::{JobRing, JobSlot, JOB_RING_DEPTH};
pub use state::{StratumState, MAX_EXTRANONCE_PREFIX};
pub use work::{OutstandingWork, Solution, WorkAssignment, MAX_EXTRANONCE_2};
pub use work_ring::{WorkRing, WORK_RING_DEPTH};
