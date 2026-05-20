//! Share submission carrying everything the adapter needs to encode
//! the wire submit.

use crate::job_id::JobId;

/// Maximum `extranonce_2` size we are willing to track per session.
/// Sets the static capacity of every `extranonce_2` buffer in the
/// stack (in particular `stratum_client::OutstandingWork`), so the
/// adapter MUST reject any session whose pool-advertised
/// `extranonce_2_size` exceeds this — otherwise we would silently
/// produce undersized shares and waste all the chip's work.
///
/// 16 bytes covers every V1 public pool we have observed (ckpool /
/// btc.com / antpool / braiins use 4–8) with comfortable margin.
/// V2 standard channels don't roll `extranonce_2`, so the size
/// lands at zero.
pub const MAX_EXTRANONCE_2: usize = 16;

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
