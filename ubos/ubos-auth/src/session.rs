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

//! Session table for GUI browser authentication.
//!
//! Sessions are created by `POST /login` after successful credential
//! validation and stored in a fixed-capacity `FnvIndexMap`. On each
//! authenticated request the session token from the `Cookie: session=`
//! header is looked up and its age is checked against `SESSION_TTL_SECS`.
//!
//! The map caps at `MAX_SESSIONS` concurrent entries. When the table is
//! full, the oldest entry (by insertion order) is evicted before
//! inserting the new one so a browser tab refresh cannot lock out
//! parallel tabs.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Instant;
use heapless::String;
use heapless::index_map::FnvIndexMap;

use crate::auth::constant_time_eq;

#[cfg(test)]
#[path = "tests/session.rs"]
mod tests;

/// Maximum number of concurrent active sessions.
const MAX_SESSIONS: usize = 8;

/// Session lifetime in seconds. Sessions older than this are
/// treated as expired even if the token is still in the table.
pub const SESSION_TTL_SECS: u64 = 86_400; // 24 hours

/// Per-session metadata stored alongside the token.
///
/// The only field kept is the creation time so TTL can be enforced
/// without a monotonic wall-clock.
pub(crate) struct SessionMeta {
    /// Firmware uptime in seconds at the moment the session was created.
    pub(crate) created_uptime_secs: u64,
}

/// Session table guarded by a critical-section mutex.
///
/// See `auth::CREDENTIALS` for the rationale on the mutex flavour:
/// `CriticalSectionRawMutex` is the portable choice across cortex-m and
/// xtensa targets, and the per-call interrupt-mask cost is negligible.
///
/// A blocking mutex, because nothing awaits while it is held. The longest
/// span is a scan of at most `MAX_SESSIONS` entries, each a 32-byte
/// compare.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
static SESSIONS: Mutex<
    CriticalSectionRawMutex,
    RefCell<FnvIndexMap<String<32>, SessionMeta, MAX_SESSIONS>>,
> = Mutex::new(RefCell::new(FnvIndexMap::new()));

/// Create a session for `token`, replacing the oldest entry when the
/// table is full.
///
/// The creation timestamp is taken from `Instant::now().as_secs()`,
/// which counts firmware uptime. Callers must generate `token` from
/// the CSPRNG via `crate::csprng::random_token`.
pub fn create_session(token: String<32>) {
    let uptime = Instant::now().as_secs();
    SESSIONS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let map = &mut *guard;

        if map.len() >= MAX_SESSIONS {
            // Note: MAX_SESSIONS caps concurrent logins on this board; a new
            // session beyond the cap silently evicts the oldest, which
            // surfaces as an unexpected logout. The bound is deliberate to keep
            // static memory fixed; revisit if more concurrent operators are expected.
            //
            // Evict the entry with the smallest uptime (oldest). The
            // `key.clone()` happens under the mutex but is a `memcpy` of a
            // 32-byte heapless `String`, not a heap allocation.
            let oldest_key = map
                .iter()
                .min_by_key(|(_, meta)| meta.created_uptime_secs)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest_key {
                map.remove(&key);
            }
        }
        // Insert can only fail if the map is still full after eviction,
        // which cannot happen because we just removed one entry.
        map.insert(
            token,
            SessionMeta {
                created_uptime_secs: uptime,
            },
        )
        .ok();
    });
}

/// Drop every session in the table.
///
/// Called when the live network address changes: the change tears down
/// the TCP connection every browser session was established over and
/// invalidates each session's CSRF token, so leaving the entries in place
/// would only let a reconnecting tab skip the login it must now perform.
/// Clearing them forces every browser back to `/login`, which is the only
/// unambiguous signal to the operator that the address change took effect.
/// Orphaned CSRF tokens are left to age out: a request whose session is
/// gone fails auth before the CSRF check is ever reached.
pub fn clear_all_sessions() {
    SESSIONS.lock(|cell| cell.borrow_mut().clear());
}

/// Return `true` if `token` exists in the table and has not expired.
///
/// Uses `constant_time_eq` on each candidate so a LAN attacker submitting
/// guessed tokens cannot oracle the stored value byte-by-byte from
/// response timing.
pub fn validate_session(token: &str) -> bool {
    let now = Instant::now().as_secs();
    SESSIONS.lock(|cell| {
        let guard = cell.borrow();
        let map = &*guard;
        // `heapless::String<N>` does not implement `Borrow<str>`, so look up
        // by iterating over entries and comparing the key with a constant-time
        // compare so the per-byte loop time does not depend on where a
        // mismatch sits.
        let mut found_valid = false;
        for (key, meta) in map {
            let matched = constant_time_eq(key.as_str(), token);
            let live = now.saturating_sub(meta.created_uptime_secs) < SESSION_TTL_SECS;
            // `matched & live` evaluates both sides for every entry so the
            // per-iteration cost does not depend on which slot holds the
            // matching token.
            found_valid |= matched & live;
        }
        found_valid
    })
}

/// Remove `token` from the session table.
///
/// Silently succeeds when the token is not present (already expired or
/// never issued). Uses `constant_time_eq` over every entry (mirroring
/// `validate_session`) so the scan time does not reveal which slot held
/// the matching token.
///
/// # Panics
///
/// Panics if a matched index does not resolve to a key on the second
/// `iter().nth()` pass. This is unreachable: the index is recorded
/// during the same locked scan, and the map is not mutated between the
/// scan and the lookup.
pub fn invalidate_session(token: &str) {
    SESSIONS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let map = &mut *guard;
        // Full scan with `enumerate`; accumulate the index rather than
        // cloning the key inside the loop. The single clone happens after
        // the scan, outside the timing-sensitive path.
        let mut matched_index: Option<usize> = None;
        for (index, (key, _)) in map.iter().enumerate() {
            if constant_time_eq(key.as_str(), token) && matched_index.is_none() {
                matched_index = Some(index);
            }
        }
        let Some(index) = matched_index else {
            return;
        };
        let key = map
            .iter()
            .nth(index)
            .map(|(key, _)| key.clone())
            .expect("BUG: matched_index is within map bounds set during scan");
        map.remove(&key);
    });
}
