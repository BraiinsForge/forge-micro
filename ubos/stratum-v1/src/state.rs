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

//! Persistent V1 session state — what the adapter remembers across
//! drive/accept cycles.
//!
//! Lives on `StratumV1Client` so it survives the borrow scope of a
//! single `accept()` call. Borrowed wire data goes here as owned
//! storage on `visit_session_update`, then gets stamped into the
//! next `MiningJob` (target, version mask) or used to build the
//! merkle root (`extranonce_1`).

use heapless::String;
use mining_protocol::hex::decode_to_slice as decode_hex;
use mining_protocol::{JobId, JobIdRing};

use crate::scratch::MAX_EXTRANONCE_1;

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

/// Maximum ASCII length of a V1 wire `job_id`. A pool matches a submit
/// by the whole id, so a longer one cut to fit is refused every time;
/// the cap leaves room above the 60-character ids one public pool sends.
pub const MAX_JOB_ID_CHARS: usize = 128;

/// Depth of the `JobId`-to-wire-id ring. Matches `stratum-client`'s
/// `JobRing` depth; the two rotate in lockstep.
pub const JOBID_MAP_DEPTH: usize = 2;

/// Default depth of [`PendingRequests`]: how many JSON-RPC requests
/// can be in flight before the slot for the oldest one gets
/// overwritten. Sized to comfortably cover the three-message
/// handshake (configure / subscribe / authorize) plus a few
/// concurrent submits at typical share rates.
pub const PENDING_DEPTH: usize = 8;

/// Kind + metadata of an outstanding JSON-RPC request.
#[derive(Debug, Clone, Copy)]
pub enum PendingRequestKind {
    Configure,
    Subscribe,
    Authorize,
    /// The pool difficulty, in diff-1-share units, in force when the
    /// share was queued. Snapshotted because `mining.set_difficulty`
    /// may change it before the response arrives, and the share's
    /// worth is what was in force when it was found; surfaced as
    /// `SubmitResult::Accepted`'s `work` value.
    Submit {
        pool_difficulty: u64,
    },
}

#[derive(Clone, Copy)]
struct PendingRequest {
    id: u32,
    kind: PendingRequestKind,
}

/// Fixed-depth map from JSON-RPC `id` to the request kind that
/// produced it. Each slot lives at `id % N`; on collision the older
/// entry is silently overwritten and its eventual response then
/// fails the id check in [`take`](Self::take) and is dropped by
/// the dispatcher.
pub struct PendingRequests<const N: usize> {
    slots: [Option<PendingRequest>; N],
    next_id: u32,
}

impl<const N: usize> PendingRequests<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; N],
            // Start at 1 so id 0, used by some libraries for
            // notifications, never collides with a registered request.
            next_id: 1,
        }
    }

    /// Reserve a fresh `id` and store `kind` in the slot at
    /// `id % N`. Any previous occupant is dropped silently; its
    /// future response will not match the id check in `take`.
    pub fn register(&mut self, kind: PendingRequestKind) -> u32 {
        let id = self.next_id;
        // `wrapping_add` so a 2^32 - request session keeps allocating;
        // at one request/sec that's ~136 years.
        self.next_id = self.next_id.wrapping_add(1);
        let index = (id as usize) % N;
        self.slots[index] = Some(PendingRequest { id, kind });
        id
    }

    /// Look up the entry at `id % N`, verify it still belongs to
    /// `id`, and remove it. Returns `None` if the slot is empty or
    /// holds a younger request that evicted the original.
    pub fn take(&mut self, id: u32) -> Option<PendingRequestKind> {
        let index = (id as usize) % N;
        let entry = self.slots[index].as_ref()?;
        if entry.id != id {
            return None;
        }
        let kind = entry.kind;
        self.slots[index] = None;
        Some(kind)
    }
}

impl<const N: usize> Default for PendingRequests<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Adapter session state: extranonce, version mask, `JobId` map,
/// and the in-flight request map.
pub struct SessionState {
    /// Binary-decoded `extranonce_1` from the subscribe response.
    /// Length valid up to `extranonce_1_len`.
    pub(crate) extranonce_1: [u8; MAX_EXTRANONCE_1],
    pub(crate) extranonce_1_len: usize,
    /// `extranonce_2` size assigned by the pool. Stamped into every
    /// `MiningJob::extranonce_2_size`.
    pub extranonce_2_size: u8,
    /// Latest target advertised by the pool; see [`PERMISSIVE_TARGET`]
    /// for the initial value.
    pub target: [u8; 32],
    /// In-flight JSON-RPC requests keyed by wire `id`; also owns id
    /// allocation.
    pub pending: PendingRequests<PENDING_DEPTH>,
    /// Ring mapping `JobId` to V1 wire `job_id` strings; see
    /// [`JOBID_MAP_DEPTH`] for why its depth is fixed.
    job_id_ring: JobIdRing<String<MAX_JOB_ID_CHARS>, JOBID_MAP_DEPTH>,
}

impl SessionState {
    /// Translate a generic `JobId` back to the V1 wire-form string.
    /// Returns `None` if the `JobId` has been evicted from the depth-2
    /// ring.
    pub(crate) fn wire_job_id(&self, generic: JobId) -> Option<&str> {
        self.job_id_ring
            .lookup(generic)
            .map(heapless::String::as_str)
    }

    /// Push the (`JobId`, wire `job_id`) pair onto the depth-2 ring.
    pub(crate) fn allocate_job_id(&mut self, wire_id: &str) -> JobId {
        let mut wire = String::<MAX_JOB_ID_CHARS>::new();
        let truncated = if wire_id.len() > MAX_JOB_ID_CHARS {
            // A longer wire id indicates a misbehaving pool; keep the
            // prefix instead of failing the job.
            &wire_id[..MAX_JOB_ID_CHARS]
        } else {
            wire_id
        };
        let _ = wire.push_str(truncated);

        self.job_id_ring.allocate(wire)
    }

    pub(crate) fn store_extranonce_1(&mut self, hex: &str) -> bool {
        let hex_bytes = hex.as_bytes();
        if hex_bytes.len() / 2 > self.extranonce_1.len() {
            return false;
        }
        let len = hex_bytes.len() / 2;
        if !decode_hex(hex_bytes, &mut self.extranonce_1[..len]) {
            return false;
        }
        self.extranonce_1_len = len;
        true
    }

    #[must_use]
    pub const fn new() -> Self {
        Self {
            extranonce_1: [0; MAX_EXTRANONCE_1],
            extranonce_1_len: 0,
            extranonce_2_size: 0,
            target: PERMISSIVE_TARGET,
            pending: PendingRequests::new(),
            job_id_ring: JobIdRing::new(),
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Initial share target before the pool sends its first
/// `mining.set_difficulty`. Two leading zero bytes corresponds to
/// pool diff 1/256² as the initial default. Once a real difficulty
/// arrives, this slot gets overwritten.
const PERMISSIVE_TARGET: [u8; 32] = {
    let mut target = [0xFFu8; 32];
    target[31] = 0x00;
    target[30] = 0x00;
    target
};
