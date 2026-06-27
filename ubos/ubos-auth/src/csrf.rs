//! CSRF token table for session-authenticated state-changing requests.
//!
//! CSRF tokens are single-use and bound to the session that issued them:
//! `issue_csrf_token` mints a fresh token and stores it alongside the
//! session id, `consume_csrf_token` removes it only when both the token
//! and the session id match. The token is delivered in the
//! `X-CSRF-Token` response header on every `GET /` for a session-
//! authenticated browser so the SPA can attach it to subsequent
//! mutating requests.
//!
//! Basic-authenticated requests bypass the check entirely — the
//! `Authorization` header already proves identity per request without
//! any state, so CSRF is not a concern for machine clients.
//!
//! The table caps at `MAX_TOKENS` concurrent entries; the oldest is
//! evicted when the table is full so a page-reload loop cannot exhaust
//! the store. Under sustained reload churn a legitimate user's pending
//! token can be evicted before they submit, in which case the next
//! mutating request fails CSRF validation and the user has to retry —
//! one extra round-trip, no security loss. A per-session quota would
//! avoid the retry but adds bookkeeping we have not needed.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Instant;
use heapless::String;
use heapless::index_map::FnvIndexMap;

use crate::auth::constant_time_eq;

#[cfg(test)]
#[path = "tests/csrf.rs"]
mod tests;

/// Maximum number of concurrent CSRF tokens.
const MAX_TOKENS: usize = 16;

/// Lifetime of a CSRF token in seconds.
pub const CSRF_TTL_SECS: u64 = 3_600; // 1 hour

/// Per-token metadata: the session that issued the token and when.
struct CsrfMeta {
    /// Session id the token was minted for. A consume call from a
    /// different session id is rejected even when the token string
    /// matches, which blocks cross-session CSRF-token replay.
    session_id: String<32>,
    /// Firmware uptime when the token was issued.
    created_uptime_secs: u64,
}

/// CSRF token table.
///
/// See `auth::CREDENTIALS` for the rationale on the mutex flavour.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
static CSRF_TOKENS: Mutex<CriticalSectionRawMutex, FnvIndexMap<String<32>, CsrfMeta, MAX_TOKENS>> =
    Mutex::new(FnvIndexMap::new());

/// Generate a CSRF token for `session_id`, store it in the table, and
/// return it.
///
/// If the table is full the oldest token (smallest creation uptime) is
/// evicted first. The returned string is suitable for the `X-CSRF-Token`
/// response header value.
///
/// # Panics
///
/// Panics with `BUG: …` if `session_id` exceeds the `String<32>` cap.
/// `session_id` is always a CSPRNG-issued 32-char string in the firmware
/// call path, so this is unreachable in practice; the assertion is here
/// to catch a future caller that passes a longer value.
pub async fn issue_csrf_token(session_id: &str) -> String<32> {
    let token = crate::csprng::random_token().await;
    let uptime = Instant::now().as_secs();
    let mut session_owned: String<32> = String::new();
    // Session ids are CSPRNG-issued 32-char strings sized to fit
    // `String<32>` exactly. A longer caller value would silently
    // truncate and produce a CSRF binding that never matches the
    // stored session id; surface that as a panic instead.
    session_owned
        .push_str(session_id)
        .expect("BUG: session_id exceeds String<32>");

    let mut guard = CSRF_TOKENS.lock().await;
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
    // The eviction above guarantees at least one free slot; if insert
    // still fails the map capacity invariant has been violated.
    .map_err(|_| ())
    .expect("BUG: insert after eviction must succeed: map capacity guaranteed by eviction above");

    token
}

/// Consume a CSRF token, returning `true` only when the token exists,
/// was issued for `session_id`, and has not expired.
///
/// A matched token is removed from the table whether or not it is still
/// valid: an expired entry is consumed and `false` is returned, so a
/// replay attempt cannot keep probing the same token. Unknown tokens
/// and session-id mismatches leave the table untouched so the issuing
/// session can still consume its own pending token.
///
/// # Panics
///
/// Panics if a matched index does not resolve to a key on the second
/// `iter().nth()` pass. This is unreachable: the index is recorded
/// during the same locked scan, and the map is not mutated between the
/// scan and the lookup.
pub async fn consume_csrf_token(session_id: &str, token: &str) -> bool {
    let now = Instant::now().as_secs();
    let mut guard = CSRF_TOKENS.lock().await;
    let map = &mut *guard;

    // Iterate every slot so the per-key compare time does not depend on
    // which slot holds the matching token. `matched_valid` is accumulated
    // unconditionally with `|=` so no short-circuit leaks position.
    // `matched_index` records the slot index rather than cloning the key
    // string, moving the heap-touching `String::clone` out of the loop
    // and into a single post-scan key lookup.
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
    // Look up the key by index after the full scan completes; the clone
    // happens once outside the timing-sensitive loop.
    let key = map
        .iter()
        .nth(index)
        .map(|(key, _)| key.clone())
        .expect("BUG: matched_index is within map bounds set during scan");
    map.remove(&key);
    matched_valid
}
