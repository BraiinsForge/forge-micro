//! Persistent V2 session state — what the adapter remembers across
//! drive/accept cycles.
//!
//! Lives on `StratumV2Client` so it survives the borrow scope of a
//! single `accept()` call. Wire-borrowed values (target, extranonce
//! prefix, prev hash, merkle root) get copied here as owned storage
//! before any visitor invocation, then reborrowed as `&[u8; 32]` /
//! `&[u8]` for the `MiningJob` and `SessionUpdate` slices the
//! visitor receives.

use mining_protocol::JobId;

/// Maximum extranonce prefix size returned by an
/// `OpenStandardMiningChannelSuccess`. The wire type `Bytes0_32`
/// caps the value at 32 bytes.
pub const MAX_EXTRANONCE_PREFIX: usize = 32;

/// Future-job slot for `NewMiningJob` with `min_ntime: None`. The
/// pool sends future jobs ahead of the matching `SetNewPrevHash`;
/// activation copies these fields into a `MiningJob` once a
/// `SetNewPrevHash` arrives whose `job_id` matches `wire_job_id`.
#[derive(Clone, Copy)]
pub struct PendingFutureJob {
    pub wire_job_id: u32,
    pub version: u32,
    pub merkle_root: [u8; 32],
}

/// Adapter session state for a V2 standard channel.
pub struct V2SessionState {
    /// `channel_id` from `OpenStandardMiningChannelSuccess`. `None`
    /// until the channel-open response arrives. The V2 submit path
    /// (lands in a later commit) reads this when it is `Some`.
    #[allow(dead_code)]
    pub channel_id: Option<u32>,
    /// Previous-block hash from the most recent `SetNewPrevHash`,
    /// in block-header byte order.
    pub prev_hash: [u8; 32],
    /// Whether `prev_hash` has been populated. The first
    /// `NewMiningJob` cannot be emitted until `SetNewPrevHash` has
    /// supplied this.
    pub have_prev_hash: bool,
    /// Most-recent share target, 256-bit little-endian. Initialised
    /// from `OpenStandardMiningChannelSuccess.target`; `SetTarget`
    /// overwrites.
    pub target: [u8; 32],
    /// `extranonce_prefix` from the channel-open success. Up to
    /// `MAX_EXTRANONCE_PREFIX` bytes; valid through
    /// `extranonce_prefix_len`.
    pub extranonce_prefix: [u8; MAX_EXTRANONCE_PREFIX],
    pub extranonce_prefix_len: usize,
    /// `nbits` from the most-recent `SetNewPrevHash`.
    pub nbits: u32,
    /// `min_ntime` from the most-recent `SetNewPrevHash` — the
    /// pool-supplied ntime base. Stored for the work generator
    /// after the `JobId`-to-wire-id ring lands with V2 submit.
    #[allow(dead_code)]
    pub ntime_base: u32,
    /// Negotiated version-rolling mask. Stays at `0` until the
    /// firmware requests rolling via `SetupConnection.flags` and
    /// the pool agrees.
    pub version_rolling_mask: u32,
    /// `JobId` counter; increments on each `visit_new_job`.
    pub next_job_id: u32,
    /// Pending future `NewMiningJob` (received with
    /// `min_ntime: None`) waiting for a matching `SetNewPrevHash`
    /// to activate it.
    pub pending_future_job: Option<PendingFutureJob>,
}

impl V2SessionState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channel_id: None,
            prev_hash: [0; 32],
            have_prev_hash: false,
            target: [0xFF; 32],
            extranonce_prefix: [0; MAX_EXTRANONCE_PREFIX],
            extranonce_prefix_len: 0,
            nbits: 0,
            ntime_base: 0,
            version_rolling_mask: 0,
            next_job_id: 0,
            pending_future_job: None,
        }
    }

    /// Allocate the next `JobId`. The V2 submit path will need a
    /// `JobId`-to-wire-id ring (mirroring V1) — that lands in a
    /// later commit.
    pub fn allocate_job_id(&mut self) -> JobId {
        let generic = JobId(self.next_job_id);
        self.next_job_id = self.next_job_id.wrapping_add(1);
        generic
    }
}

impl Default for V2SessionState {
    fn default() -> Self {
        Self::new()
    }
}
