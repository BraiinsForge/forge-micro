//! Response shape for `GET /api/system/scoreboard`.
//!
//! Top-N best-difficulty shares since boot. The response body is a
//! JSON array of `SystemScoreboardEntry` values. Until the
//! stratum-client `visit_submit_result` hook lands, the array is
//! always empty; the shape is stable so external clients can poll
//! without schema negotiation.

use heapless::String;
use serde::Serialize;

/// One entry in the best-shares scoreboard.
///
/// Fields match the stratum protocol metadata for the solution so
/// clients can reconstruct and verify the block candidate if needed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemScoreboardEntry {
    /// Position in the ranked list, starting at 1.
    pub rank: u8,
    /// Seconds elapsed since this share was found.
    pub since_seconds: u64,
    /// Share difficulty as an integer (not compact target).
    pub difficulty: u64,
    /// Stratum job identifier for this solution.
    pub job_id: u32,
    /// Extranonce2 value used when the solution was found, hex-encoded.
    pub extranonce2: String<32>,
    /// `nTime` field from the solved block header.
    pub ntime: u32,
    /// Winning nonce value.
    pub nonce: u32,
    /// Version-rolling bits applied to the block header.
    pub version_bits: u32,
}
