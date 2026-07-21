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

//! Credential store and request authentication.
//!
//! Two authentication paths are supported:
//!
//! - **HTTP Basic** (`Authorization: Basic <base64>`): suited for machine
//!   clients like Braiins Deck. The header rides every request without a
//!   login round-trip.
//! - **Session cookie** (`Cookie: session=<token>`): used by the web GUI
//!   after a successful `POST /login`. The session table lives in
//!   `crate::session` and is populated when the login form is submitted.
//! - **Anonymous**: no credentials present. Routes that require auth
//!   return a `401 WWW-Authenticate: Basic realm="ubos"` response.
//!
//! The single credential store (`CREDENTIALS`) is seeded from the
//! persisted `config.api` section at boot via [`set_credentials`] and
//! refreshed whenever the config watch publishes a new value. A
//! `POST /api/config` change therefore takes effect at runtime and
//! persists across reboots.
//!
//! ## Known limitations
//!
//! No per-IP failed-login rate limit. Bounded by the LAN allow-list.
//! A future improvement is a counter-with-backoff after N failures.
//!
//! Credentials stored and compared as plaintext. `no_std` PBKDF2/argon2 is
//! not currently available; a per-board-serial-salted SHA-256 would harden
//! flash-readout, but full hashing is a future redesign.
//!
//! Eviction picks the oldest session. An attacker with valid credentials
//! can fill the session table to flush legitimate sessions. Bounded by
//! allow-list.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use heapless::String;
use portable_atomic::{AtomicBool, Ordering};

use ubos_logging::warn;

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;

/// Default username applied at boot.
pub const DEFAULT_USERNAME: &str = "root";
/// Default password applied at boot.
pub const DEFAULT_PASSWORD: &str = "root";

/// Maximum length for stored credentials.
const CREDENTIAL_MAX_LEN: usize = 32;

/// Dummy username used on the `BadFormat` path of `authenticate_basic` to
/// ensure a constant number of `constant_time_eq` calls regardless of
/// whether the header parsed successfully.
///
/// Both dummies are exactly `CREDENTIAL_MAX_LEN` bytes long so the scan
/// inside `constant_time_eq` takes the same number of iterations as a
/// real credential comparison.
const DUMMY_USERNAME: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
/// Dummy password used on the `BadFormat` path of `authenticate_basic`.
const DUMMY_PASSWORD: &str = "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy";

/// Stack-buffer size used by [`constant_time_eq`].
///
/// Every secret compared by this function — usernames, passwords, CSRF
/// tokens, and session tokens — must fit in this many bytes. Credentials
/// are bounded by `CREDENTIAL_MAX_LEN` at their setup sites; CSRF and
/// session tokens are exactly 32 ASCII characters by construction in
/// `csprng::random_token`. Raising the bound for any one secret type
/// requires bumping this constant too, or `constant_time_eq` will silently
/// truncate the longer input.
const SECRET_MAX_LEN: usize = 32;

/// Stack buffer for the base64-decoded `Authorization: Basic` payload.
///
/// Sized for `username:password` with both fields at `CREDENTIAL_MAX_LEN`
/// plus the colon separator. Longer payloads are rejected as `BadFormat`
/// at `base64_decode` rather than truncated.
const BASIC_AUTH_DECODE_BUFFER_LEN: usize = CREDENTIAL_MAX_LEN * 2 + 1;

/// A username and password pair.
///
/// Both fields are bounded `heapless::String` so the struct lives in a
/// static mutex without heap allocation.
#[derive(Clone)]
pub struct Credentials {
    /// The login username.
    pub username: String<CREDENTIAL_MAX_LEN>,
    /// The login password.
    pub password: String<CREDENTIAL_MAX_LEN>,
}

impl Credentials {
    /// Construct an empty credential pair for const initialization.
    ///
    /// Empty `username` is the in-band marker for "not yet populated";
    /// the HTTP listeners replace it with the persisted `config.api`
    /// credentials before serving.
    const fn const_empty() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
        }
    }
}

/// Shared, runtime-replaceable credential store.
///
/// Guarded by `CriticalSectionRawMutex` rather than the lighter-weight
/// `ThreadModeRawMutex` because `ThreadModeRawMutex` is cortex-m-only
/// and this crate ships on the Xtensa esp32s3 target as well. A
/// `NoopRawMutex` would work for the current single-task access pattern
/// on each board but would silently break if a future maintainer added
/// cross-core access; `CriticalSectionRawMutex` is the portable safe
/// default and the per-request overhead is negligible at HTTP rates.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub(crate) static CREDENTIALS: Mutex<CriticalSectionRawMutex, Credentials> =
    Mutex::new(Credentials::const_empty());

/// Tracks whether the default-credentials warning has already been
/// emitted so the log is not spammed on every request.
static DEFAULT_CREDS_WARNED: AtomicBool = AtomicBool::new(false);

/// Populate `CREDENTIALS` with the persisted `config.api` credentials.
///
/// Clears both fields and replaces them with the supplied values.
/// `ApiConfig` bounds both fields to `CREDENTIAL_MAX_LEN` at the
/// update site, so `push_str` cannot overflow.
///
/// Called once at boot with the loaded config and again on each config
/// watch publish so `POST /api/config` credential changes take effect
/// at runtime and persist across reboots.
pub async fn set_credentials(username: &str, password: &str) {
    let mut guard = CREDENTIALS.lock().await;
    guard.username.clear();
    guard.password.clear();
    // ApiConfig bounds both fields to CREDENTIAL_MAX_LEN, so push_str
    // cannot return Err here.
    let _ = guard.username.push_str(username);
    let _ = guard.password.push_str(password);
}

/// Populate `CREDENTIALS` with the default `root`/`root` pair if the
/// store is empty.
///
/// Seeds the default pair into an empty store and leaves a populated
/// store untouched. Retained for the auth test suite and as the
/// empty-store fallback; the production listeners seed from
/// `config.api` via `set_credentials`.
///
/// # Panics
///
/// Panics with `BUG: …` if `DEFAULT_USERNAME` or `DEFAULT_PASSWORD`
/// exceeds `CREDENTIAL_MAX_LEN`. Both constants are shorter than the
/// limit by construction; the assertion is unreachable in practice.
pub async fn init_credentials() {
    let mut guard = CREDENTIALS.lock().await;
    if guard.username.is_empty() {
        // DEFAULT_USERNAME and DEFAULT_PASSWORD fit in CREDENTIAL_MAX_LEN.
        guard
            .username
            .push_str(DEFAULT_USERNAME)
            .expect("BUG: DEFAULT_USERNAME exceeds CREDENTIAL_MAX_LEN");
        guard
            .password
            .push_str(DEFAULT_PASSWORD)
            .expect("BUG: DEFAULT_PASSWORD exceeds CREDENTIAL_MAX_LEN");
    }
}

/// Emit a boot warning when the credential store still holds the default
/// `root`/`root` pair.
///
/// The warning is gated by `DEFAULT_CREDS_WARNED` so it fires at most
/// once per boot regardless of how many times this function is called.
/// Call this after `set_credentials` or `init_credentials` so the check
/// sees the credentials that will actually be used.
pub async fn warn_if_default_credentials() {
    let guard = CREDENTIALS.lock().await;
    if guard.username.as_str() == DEFAULT_USERNAME
        && guard.password.as_str() == DEFAULT_PASSWORD
        && !DEFAULT_CREDS_WARNED.swap(true, Ordering::Relaxed)
    {
        warn!(
            "default credentials root/root are in use; \
             change them via POST /api/config"
        );
    }
}

/// Snapshot of the current credential pair.
///
/// Both HTTP listeners seed the store via `set_credentials` before
/// their accept loop, so it is guaranteed populated by the time this
/// runs.
pub async fn current_credentials() -> Credentials {
    CREDENTIALS.lock().await.clone()
}

/// The authentication context carried forward after a successful check.
///
/// Passed to downstream handlers so they can branch on the auth method
/// (e.g. to bypass CSRF for `Basic`-authenticated requests).
pub enum AuthCtx {
    /// Authenticated via `Authorization: Basic`.
    Basic,
    /// Authenticated via a session cookie. Carries the token so the
    /// CSRF check and logout handler can reference the active session.
    Session(String<32>),
    /// No credentials presented. Only permitted for paths that are
    /// explicitly allowed without authentication.
    Anonymous,
}

/// Why authentication failed.
pub enum AuthError {
    /// The `Authorization: Basic` header value is not valid base64 or
    /// does not contain a `:` separator.
    BadFormat,
    /// Credentials were present but did not match the stored pair.
    BadCredentials,
}

/// Attempt to authenticate using pre-extracted header values.
///
/// Tries `Authorization: Basic` first (via `authorization`), then the
/// `session` cookie (via `cookie`). Returns `Anonymous` when neither is
/// present.
///
/// Accepting raw `Option<&str>` values instead of a typed request object
/// keeps this function independent of any transport type; callers extract
/// the header strings and pass them directly, and tests can call this
/// with plain string literals.
///
/// # Errors
///
/// Returns `AuthError::BadFormat` if the `Authorization` header is
/// malformed. Returns `AuthError::BadCredentials` if the credentials
/// do not match the stored pair.
///
/// # Panics
///
/// Panics with `BUG: …` if `validate_session` accepts a token whose
/// length exceeds `String<32>` — this is unreachable because
/// `validate_session` only returns `true` for tokens already stored
/// in the fixed-capacity session table.
pub async fn authenticate(
    authorization: Option<&str>,
    cookie: Option<&str>,
) -> Result<AuthCtx, AuthError> {
    if let Some(authorization_value) = authorization {
        return authenticate_basic(authorization_value).await;
    }
    if let Some(token) = extract_session_cookie_from_str(cookie)
        && crate::session::validate_session(token).await
    {
        let mut owned = String::new();
        // `validate_session` only returns true for tokens already
        // stored in the session table; stored tokens are exactly 32
        // chars by `csprng::random_token`. A longer cookie value
        // cannot have validated, so this push fits by construction.
        owned
            .push_str(token)
            .expect("BUG: validated session token exceeds String<32>");
        return Ok(AuthCtx::Session(owned));
    }
    // Session cookie present but expired or unknown — treat as
    // anonymous so the caller redirects to /login.
    Ok(AuthCtx::Anonymous)
}

/// Authenticate an `Authorization: Basic <base64>` value.
///
/// Decodes the base64 payload and compares against `CREDENTIALS`. On any
/// parse failure two `constant_time_eq` calls against fixed dummy buffers
/// are still executed so the response time does not leak whether the
/// header failed to parse or failed credential comparison.
async fn authenticate_basic(authorization: &str) -> Result<AuthCtx, AuthError> {
    // Decode buffer lives in the outer frame so slices into it can be
    // returned from the inner parse block without lifetime issues.
    let mut decoded = [0u8; BASIC_AUTH_DECODE_BUFFER_LEN];

    // Attempt to parse the header. The `Option` captures success or
    // failure without an early return so the credential comparison
    // block below runs unconditionally.
    let parse_result: Option<(usize, usize)> = (|| {
        let encoded = authorization.strip_prefix("Basic ")?.trim();
        let length = base64_decode(encoded.as_bytes(), &mut decoded)?;
        let text = core::str::from_utf8(&decoded[..length]).ok()?;
        let colon = text.find(':')?;
        Some((colon, length))
    })();

    // Always fetch credentials and run exactly two constant-time compares
    // regardless of whether parsing succeeded. This prevents response-time
    // variation from leaking whether the failure was a parse error or a
    // credential mismatch.
    let stored = current_credentials().await;
    let (supplied_username, supplied_password) = match parse_result {
        Some((colon, length)) => {
            // `decoded` was written by `base64_decode` above; `length` and
            // `colon` are bounds checked inside the closure. The UTF-8
            // check passed, so indexing by `colon` and `length` is safe.
            let text = core::str::from_utf8(&decoded[..length])
                .expect("BUG: UTF-8 validated in closure above");
            (&text[..colon], &text[colon + 1..])
        }
        None => (DUMMY_USERNAME, DUMMY_PASSWORD),
    };
    // Constant-time compare on both fields so a LAN-attached attacker
    // cannot time HTTP responses to learn the credential byte-by-byte.
    let username_ok = constant_time_eq(stored.username.as_str(), supplied_username);
    let password_ok = constant_time_eq(stored.password.as_str(), supplied_password);

    match parse_result {
        None => {
            // Discard the comparison results so the optimizer cannot prove
            // they are unused and elide the calls.
            let _ = core::hint::black_box(username_ok & password_ok);
            Err(AuthError::BadFormat)
        }
        Some(_) => {
            if username_ok & password_ok {
                Ok(AuthCtx::Basic)
            } else {
                Err(AuthError::BadCredentials)
            }
        }
    }
}

/// Compare two strings in fixed time independent of input length.
///
/// Both inputs are copied into `SECRET_MAX_LEN`-sized buffers padded
/// with zeros, then XOR'd byte-by-byte into a single accumulator without
/// short-circuiting. The length difference is mixed into the accumulator
/// after the loop so different lengths never report equal, but the byte
/// loop runs over the full buffer regardless of either input's length.
///
/// Returns `false` immediately (after the full-buffer scan) when either
/// input length exceeds `SECRET_MAX_LEN`. This self-defence prevents a
/// future call site from accidentally passing an oversize secret that
/// shares a `SECRET_MAX_LEN`-byte prefix with a stored value and would
/// otherwise compare equal after truncation.
///
/// Every secret compared by current callers fits within `SECRET_MAX_LEN`:
/// credentials are bounded by `CREDENTIAL_MAX_LEN` at their setup sites,
/// and CSRF and session tokens are exactly 32 ASCII characters by
/// `csprng::random_token` construction.
#[must_use]
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    // Record whether either input is oversize before touching the buffers
    // so the guard does not short-circuit the constant-time scan below.
    let oversize = (left.len() > SECRET_MAX_LEN) | (right.len() > SECRET_MAX_LEN);
    let mut left_buffer = [0u8; SECRET_MAX_LEN];
    let mut right_buffer = [0u8; SECRET_MAX_LEN];
    let left_len = left.len().min(SECRET_MAX_LEN);
    let right_len = right.len().min(SECRET_MAX_LEN);
    left_buffer[..left_len].copy_from_slice(&left.as_bytes()[..left_len]);
    right_buffer[..right_len].copy_from_slice(&right.as_bytes()[..right_len]);
    let mut diff: usize = 0;
    for (left_byte, right_byte) in left_buffer.iter().zip(right_buffer.iter()) {
        diff |= usize::from(left_byte ^ right_byte);
    }
    // Length mismatch is folded in after the loop so it never short-circuits.
    diff |= left.len() ^ right.len();
    // Oversize inputs are rejected; the scan above still runs in full so
    // the execution time does not reveal whether the oversize branch was hit.
    !oversize && diff == 0
}

/// Extract the `session` cookie value from a raw `Cookie` header value.
///
/// Returns `None` when `cookie_header` is `None`, when the header is
/// absent, or when the `session` key is not found in the cookie string.
#[must_use]
pub fn extract_session_cookie_from_str(cookie_header: Option<&str>) -> Option<&str> {
    let cookie_header = cookie_header?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        let Some(equals) = pair.find('=') else {
            continue;
        };
        let name = pair[..equals].trim();
        if name == "session" {
            return Some(pair[equals + 1..].trim());
        }
    }
    None
}

/// Decode `input` bytes from standard base64 alphabet into `output`.
///
/// Returns the number of decoded bytes on success, or `None` on an
/// invalid character or truncated input. Does not require `std` or a
/// crate dependency.
fn base64_decode(input: &[u8], output: &mut [u8]) -> Option<usize> {
    /// Map a base64 character to its 6-bit value, or return `None`.
    ///
    /// `=` is rejected here; padding is handled by the caller after
    /// counting the trailing run, so an `=` outside the final 0–2 byte
    /// run is treated as a syntax error instead of silently decoding to
    /// zero.
    fn decode_char(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let input_stripped: &[u8] = input.trim_ascii_end();
    let padding = input_stripped
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'=')
        .count();
    let chunks = input_stripped.len() / 4;
    if !input_stripped.len().is_multiple_of(4) {
        return None;
    }
    // RFC 4648 allows at most two trailing `=` characters, and only in
    // the final chunk. Anything else is malformed input.
    if padding > 2 {
        return None;
    }
    let decoded_len = chunks * 3 - padding;
    if output.len() < decoded_len {
        return None;
    }
    let mut output_pos = 0;
    let last_chunk_index = chunks.saturating_sub(1);
    for (chunk_index, chunk) in input_stripped.chunks_exact(4).enumerate() {
        // Only the final chunk may contain `=` padding bytes, and only
        // in its last one or two positions. `decode_char` rejects `=`
        // unconditionally; substitute zero for the padded slots and
        // reject any `=` that appears outside the allowed positions.
        let on_last = chunk_index == last_chunk_index;
        let mut bytes = [0u8; 4];
        for (slot_index, &raw) in chunk.iter().enumerate() {
            if raw == b'=' {
                // Allowed only in the last chunk's last `padding` slots.
                let pad_start = 4 - padding;
                if !on_last || slot_index < pad_start {
                    return None;
                }
                bytes[slot_index] = 0;
            } else {
                bytes[slot_index] = decode_char(raw)?;
            }
        }
        let triple = (u32::from(bytes[0]) << 18)
            | (u32::from(bytes[1]) << 12)
            | (u32::from(bytes[2]) << 6)
            | u32::from(bytes[3]);
        if output_pos < decoded_len {
            // Bits 23..16 of triple; shift guarantees the value is < 256.
            #[allow(clippy::cast_possible_truncation)]
            {
                output[output_pos] = (triple >> 16) as u8;
            }
            output_pos += 1;
        }
        if output_pos < decoded_len {
            // Bits 15..8 of triple; masking via shift guarantees < 256.
            #[allow(clippy::cast_possible_truncation)]
            {
                output[output_pos] = (triple >> 8) as u8;
            }
            output_pos += 1;
        }
        if output_pos < decoded_len {
            // Bits 7..0 of triple; masking via `& 0xff` guarantees < 256.
            #[allow(clippy::cast_possible_truncation)]
            {
                output[output_pos] = triple as u8;
            }
            output_pos += 1;
        }
    }
    Some(decoded_len)
}
