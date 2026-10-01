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

//! CSRF tokens: single-use, bound to the session that issued them, and
//! delivered in the `X-CSRF-Token` header of every session `GET /`. A
//! full table evicts its oldest token; a user evicted under reload churn
//! retries once, with no security loss. Scans visit every slot; see the
//! `auth` module doc.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Instant;
use heapless::String;
use heapless::index_map::FnvIndexMap;

use crate::auth::constant_time_eq;

#[cfg(test)]
#[path = "tests/csrf.rs"]
mod tests;

const MAX_TOKENS: usize = 16;

pub const CSRF_TTL_SECS: u64 = 3_600;

struct CsrfMeta {
    /// A consume from another session is rejected even on a matching
    /// token, which blocks cross-session replay.
    session_id: String<32>,
    created_uptime_secs: u64,
}

static CSRF_TOKENS: Mutex<
    CriticalSectionRawMutex,
    RefCell<FnvIndexMap<String<32>, CsrfMeta, MAX_TOKENS>>,
> = Mutex::new(RefCell::new(FnvIndexMap::new()));

/// Mint a token for `session_id`, evicting the oldest when the table is
/// full.
///
/// # Panics
///
/// If `session_id` exceeds `String<32>`: a longer id would truncate into
/// a binding that never matches.
pub fn issue_csrf_token(session_id: &str) -> String<32> {
    let token = crate::csprng::random_token();
    let uptime = Instant::now().as_secs();
    let mut session_owned: String<32> = String::new();
    session_owned
        .push_str(session_id)
        .expect("BUG: session_id exceeds String<32>");

    CSRF_TOKENS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let map = &mut *guard;

        if map.len() >= MAX_TOKENS {
            let oldest_key = map
                .iter()
                .min_by_key(|(_, meta)| meta.created_uptime_secs)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest_key {
                map.remove(&key);
            }
        }
        map.insert(
            token.clone(),
            CsrfMeta {
                session_id: session_owned,
                created_uptime_secs: uptime,
            },
        )
        .map_err(|_| ())
        .expect(
            "BUG: insert after eviction must succeed: map capacity guaranteed by eviction above",
        );
    });

    token
}

/// Consume a token: `true` only when it exists, was issued for
/// `session_id` and is unexpired. A matched expired token is removed too,
/// so a replay cannot keep probing it; a mismatch leaves the table alone,
/// so the issuing session can still consume it.
///
/// # Panics
///
/// If the matched index does not resolve to a key, which cannot happen
/// under one lock.
pub fn consume_csrf_token(session_id: &str, token: &str) -> bool {
    let now = Instant::now().as_secs();
    CSRF_TOKENS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let map = &mut *guard;

        // `|=`, not a break: no short-circuit leaks the slot. The index,
        // not the key, is kept in the loop.
        let mut matched_index: Option<usize> = None;
        let mut matched_valid = false;
        for (index, (key, meta)) in map.iter().enumerate() {
            let token_match = constant_time_eq(key.as_str(), token);
            let session_match = constant_time_eq(meta.session_id.as_str(), session_id);
            let live = now.saturating_sub(meta.created_uptime_secs) < CSRF_TTL_SECS;
            matched_valid |= token_match & session_match & live;
            if token_match & session_match && matched_index.is_none() {
                matched_index = Some(index);
            }
        }

        let Some(index) = matched_index else {
            return false;
        };
        let key = map
            .iter()
            .nth(index)
            .map(|(key, _)| key.clone())
            .expect("BUG: matched_index is within map bounds set during scan");
        map.remove(&key);
        matched_valid
    })
}
