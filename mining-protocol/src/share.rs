//! Share submission carrying everything the adapter needs to encode
//! the wire submit.

use crate::job_id::JobId;

/// One found share, headed for the pool via `MiningClient::submit`.
///
/// `id` references the stratum job the share belongs to (looked up
/// in the `JobRing` on the `stratum-client` side; translated to wire
/// form by the adapter). `extranonce_2` is the bytes the miner used
/// when assembling the coinbase that produced this nonce — empty for
/// V2 standard channels (the channel's `extranonce_prefix` is the
/// only contribution there).
#[derive(Debug, Clone, Copy)]
pub struct Share<'a> {
    pub id: JobId,
    pub nonce: u32,
    pub ntime: u32,
    pub version_bits: u32,
    pub extranonce_2: &'a [u8],
}
