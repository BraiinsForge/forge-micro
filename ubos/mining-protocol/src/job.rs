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

//! Borrowed `MiningJob` exposed to the visitor.

use crate::job_id::JobId;
use crate::lazy_merkle::LazyMerkle;

/// A mining job in canonical, ready-to-use form.
///
/// All hash-shaped fields are in block-header byte order: a direct
/// memcpy of `prev_hash` lands at `header[4..36]`, the root computed
/// from `merkle` lands at `header[36..68]`. The V1 adapter does the
/// per-u32 `prev_hash` swap internally; consumers do not see the
/// V1 quirk.
///
/// `target` is the 32-byte little-endian 256-bit integer the solver
/// compares the `sha256d(header)` output against (`target[0]` is the
/// least-significant byte).
///
/// All slices borrow from the adapter's wire/decode buffers and are
/// valid only for the duration of one `accept()` call. Visitors
/// must copy out anything they need to keep.
#[derive(Debug, Clone, Copy)]
pub struct MiningJob<'a> {
    /// Adapter-assigned correlation id; opaque to consumers.
    pub id: JobId,
    pub prev_hash: &'a [u8; 32],
    pub target: &'a [u8; 32],
    pub version: u32,
    /// Compact difficulty target.
    pub nbits: u32,
    /// Pool-supplied `ntime` base; the work generator rolls forward
    /// from here within the consensus-allowed window.
    pub ntime: u32,
    /// Whether this notify invalidates older queued jobs.
    pub clean_jobs: bool,
    /// Bytes of `extranonce_2` the miner is expected to roll. `0`
    /// means no extranonce rolling (V2 standard channels).
    pub extranonce_2_size: u8,
    /// Materials for the merkle root (V1) or the precomputed root
    /// itself (V2 standard).
    pub merkle: LazyMerkle<'a>,
}
