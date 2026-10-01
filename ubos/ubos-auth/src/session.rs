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

//! Session table for browser authentication. A full table evicts its
//! oldest entry, so a tab refresh cannot lock out parallel tabs; an
//! attacker with valid credentials can flush legitimate sessions, and
//! the allow-list is the bound on that. Scans visit every slot; see the
//! `auth` module doc.

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

const MAX_SESSIONS: usize = 8;

/// Session lifetime; an older entry is expired even while in the table.
pub const SESSION_TTL_SECS: u64 = 86_400;

pub(crate) struct SessionMeta {
    /// Firmware uptime in seconds at creation, so the TTL needs no wall
    /// clock.
    pub(crate) created_uptime_secs: u64,
}

static SESSIONS: Mutex<
    CriticalSectionRawMutex,
    RefCell<FnvIndexMap<String<32>, SessionMeta, MAX_SESSIONS>>,
> = Mutex::new(RefCell::new(FnvIndexMap::new()));

/// Create a session for `token`, a `csprng::random_token`, evicting the
/// oldest entry when the table is full.
pub fn create_session(token: String<32>) {
    let uptime = Instant::now().as_secs();
    SESSIONS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let map = &mut *guard;

        if map.len() >= MAX_SESSIONS {
            let oldest_key = map
                .iter()
                .min_by_key(|(_, meta)| meta.created_uptime_secs)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest_key {
                map.remove(&key);
            }
        }
        map.insert(
            token,
            SessionMeta {
                created_uptime_secs: uptime,
            },
        )
        .ok();
    });
}

/// Drop every session. Called on a live address change, so every browser
/// returns to `/login`, which is the one unambiguous sign the change
/// took effect. Orphaned CSRF tokens age out: a request without its
/// session fails auth first.
pub fn clear_all_sessions() {
    SESSIONS.lock(|cell| cell.borrow_mut().clear());
}

/// `token` is in the table and not expired.
pub fn validate_session(token: &str) -> bool {
    let now = Instant::now().as_secs();
    SESSIONS.lock(|cell| {
        let guard = cell.borrow();
        let map = &*guard;
        let mut found_valid = false;
        for (key, meta) in map {
            let matched = constant_time_eq(key.as_str(), token);
            let live = now.saturating_sub(meta.created_uptime_secs) < SESSION_TTL_SECS;
            // `&`, not `&&`: both sides evaluate for every entry.
            found_valid |= matched & live;
        }
        found_valid
    })
}

/// Remove `token`; a token not present is no error.
///
/// # Panics
///
/// If the matched index does not resolve to a key, which cannot happen
/// under one lock.
pub fn invalidate_session(token: &str) {
    SESSIONS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let map = &mut *guard;
        // The index, not the key, is kept in the loop; the one clone runs
        // after the scan.
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
