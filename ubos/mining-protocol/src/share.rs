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

//! Share submission carrying everything the adapter needs to encode
//! the wire submit.

use crate::job_id::JobId;

/// Maximum `extranonce_2` size tracked per session: the static
/// capacity of every `extranonce_2` buffer in the stack, in particular
/// `stratum_client::OutstandingWork`. The adapter MUST reject any
/// session whose pool-advertised `extranonce_2_size` exceeds this, or
/// it would silently produce undersized shares and waste the chip's
/// work.
///
/// V1 public pools use 4–8 bytes; 16 leaves comfortable margin.
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
