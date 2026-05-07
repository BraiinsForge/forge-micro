//! Persistent V1 session state — what the adapter remembers across
//! drive/accept cycles.
//!
//! Lives on `StratumV1Client` so it survives the borrow scope of a
//! single `accept()` call. Borrowed wire data goes here as owned
//! storage on `visit_session_update`, then gets stamped into the
//! next `MiningJob` (target, version mask) or used to build the
//! merkle root (`extranonce_1`).

use heapless::String;
use mining_protocol::JobId;

use crate::scratch::MAX_EXTRANONCE_1;

/// Maximum ASCII length of a V1 wire `job_id`. Public pools we have
/// observed stay well under this; ckpool emits 4–8 char hex.
pub const MAX_JOB_ID_CHARS: usize = 32;

/// Depth of the `JobId`-to-wire-id ring. Matches `stratum-client`'s
/// `JobRing` depth; the two rotate in lockstep.
pub const JOBID_MAP_DEPTH: usize = 2;

/// One row of the JobId-to-wire-id translation ring.
pub struct JobIdSlot {
    pub generic: JobId,
    pub wire: String<MAX_JOB_ID_CHARS>,
}

/// Adapter session state: extranonce, version mask, `JobId` map,
/// and the JSON-RPC sequence-id counter for outgoing requests.
pub struct SessionState {
    /// Binary-decoded `extranonce_1` from the subscribe response.
    /// Length valid up to `extranonce_1_len`.
    pub extranonce_1: [u8; MAX_EXTRANONCE_1],
    pub extranonce_1_len: usize,
    /// `extranonce_2` size assigned by the pool. Stamped into every
    /// `MiningJob::extranonce_2_size`.
    pub extranonce_2_size: u8,
    /// Negotiated version-rolling mask. `0` until the configure
    /// response lands.
    pub version_rolling_mask: u32,
    /// Latest target advertised by the pool. Initial value is the
    /// most-permissive target; `mining.set_difficulty` overwrites.
    pub target: [u8; 32],
    /// Monotonic counter for outbound JSON-RPC `id` field. Starts at
    /// 1; `0` is reserved for "no id assigned yet".
    pub next_request_id: u32,
    /// `JobId` counter; assigned in `visit_new_job` translation.
    pub next_job_id: u32,
    /// Depth-`JOBID_MAP_DEPTH` ring mapping `JobId` to V1 wire
    /// `job_id` strings. Rotates in lockstep with
    /// `stratum-client`'s `JobRing`.
    pub job_id_ring: [Option<JobIdSlot>; JOBID_MAP_DEPTH],
    pub job_id_write_index: usize,
}

impl SessionState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            extranonce_1: [0; MAX_EXTRANONCE_1],
            extranonce_1_len: 0,
            extranonce_2_size: 0,
            version_rolling_mask: 0,
            target: PERMISSIVE_TARGET,
            next_request_id: 1,
            next_job_id: 0,
            job_id_ring: [const { None }; JOBID_MAP_DEPTH],
            job_id_write_index: 0,
        }
    }

    /// Reserve and return the next outbound JSON-RPC `id`.
    pub fn allocate_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        id
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Initial share target before the pool sends its first
/// `mining.set_difficulty`. Two leading zero bytes corresponds to
/// pool diff 1/256² — the same default `ubos-backend` used. Once a
/// real difficulty arrives, this slot gets overwritten.
const PERMISSIVE_TARGET: [u8; 32] = {
    let mut target = [0xFFu8; 32];
    target[31] = 0x00;
    target[30] = 0x00;
    target
};
