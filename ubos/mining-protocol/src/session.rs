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

//! Mid-session reconfiguration events.
//!
//! These flow through `visit_session_update` whenever the pool changes
//! something that is *not* a job — target, extranonce, version mask,
//! or a reconnect request. The same variants are also used during
//! initial session setup (the visitor sees a sequence of
//! `SessionUpdate` events before the first `visit_new_job`).

/// BIP320 version-rolling mask: the 16 bits between bit 13 and bit 28,
/// inclusive, that a miner may flip without invalidating consensus
/// rules. V1 requests it in `mining.configure` and V2 grants it when
/// the upstream advertises version rolling; it is distinct from any
/// bitmask a specific ASIC backend rolls in hardware.
pub const BIP320_VERSION_MASK: u32 = 0x1FFF_E000;

/// Pool-driven session reconfiguration.
///
/// Borrowed slices are valid only for the duration of the `accept()`
/// call that produced the event; the visitor copies anything it needs
/// to keep.
#[derive(Debug, Clone, Copy)]
pub enum SessionUpdate<'a> {
    /// New share target, 256-bit little-endian.
    SetTarget(&'a [u8; 32]),
    /// New extranonce assignment. V1 emits this on the `subscribe`
    /// response and on `mining.set_extranonce`; V2 emits it during
    /// the channel-open success and (for extended channels) on
    /// `SetExtranoncePrefix`.
    SetExtranonce { prefix: &'a [u8], en2_size: u8 },
    /// New version-rolling mask. V1-only post-handshake; V2 derives
    /// it once from `SetupConnection.flags`.
    SetVersionMask(u32),
    /// Pool requests the client reconnect to a different endpoint.
    Reconnect {
        host: &'a str,
        port: u16,
        wait_seconds: u16,
    },
}

/// Result of a previously submitted `Share`, surfaced to the visitor
/// via `visit_submit_result`.
///
/// The two protocols report acceptance differently: V1 answers each
/// submit individually with a boolean result, while V2's
/// `SubmitSharesSuccess` covers a range of sequence numbers and
/// reports both a count and a pool-credited share-weight sum.
/// `count` is `1` per V1 response and whatever the V2 pool
/// acknowledged; `work` is in diff-1-share units — the pool difficulty
/// at submit time for V1, SV2's `new_shares_sum` for V2.
#[derive(Debug, Clone, Copy)]
pub enum SubmitResult<'a> {
    Accepted {
        count: u32,
        work: u64,
    },
    Rejected {
        error_code: Option<u32>,
        error_message: Option<&'a str>,
    },
}
