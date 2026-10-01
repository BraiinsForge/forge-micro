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

//! Credential store and request authentication: HTTP Basic for machine
//! clients, a session cookie for the web GUI, anonymous otherwise. The
//! store is seeded from `config.api` and refreshed on every config
//! publish.
//!
//! Every secret compare in this crate runs in constant time over a
//! fixed-size buffer, and every table scan visits every slot, so a
//! LAN-attached attacker cannot time a response to learn a secret or
//! which slot holds a token. Credentials are stored as plaintext
//! because no `no_std` PBKDF2 or argon2 implementation is available;
//! there is no failed-login rate limit, and the allow-list is the bound
//! on both.
//!
//! Every mutex in this crate is a blocking `CriticalSectionRawMutex`:
//! nothing awaits under one, and that flavour is the one that is `Sync`
//! on both the cortex-m and the Xtensa targets.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use heapless::String;
use portable_atomic::{AtomicBool, Ordering};

use ubos_logging::warn;

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;

pub const DEFAULT_USERNAME: &str = "root";
pub const DEFAULT_PASSWORD: &str = "root";

const CREDENTIAL_MAX_LEN: usize = 32;

/// Compared on the `BadFormat` path of `authenticate_basic`, so a parse
/// failure costs the same two compares as a credential mismatch. Both are
/// `CREDENTIAL_MAX_LEN` long.
const DUMMY_USERNAME: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
const DUMMY_PASSWORD: &str = "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy";

/// Buffer size of [`constant_time_eq`]. Every secret it compares,
/// credentials and the two token kinds, must fit; a longer one is
/// rejected, never truncated.
const SECRET_MAX_LEN: usize = 32;

/// Decoded `Authorization: Basic` payload: `username:password` with both
/// at `CREDENTIAL_MAX_LEN`. A longer payload is `BadFormat`.
const BASIC_AUTH_DECODE_BUFFER_LEN: usize = CREDENTIAL_MAX_LEN * 2 + 1;

/// A username and password pair.
#[derive(Clone)]
pub struct Credentials {
    pub username: String<CREDENTIAL_MAX_LEN>,
    pub password: String<CREDENTIAL_MAX_LEN>,
}

impl Credentials {
    /// An empty `username` marks a store not yet populated.
    const fn const_empty() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
        }
    }
}

/// The credential store; see the module doc for the mutex flavour.
pub(crate) static CREDENTIALS: Mutex<CriticalSectionRawMutex, RefCell<Credentials>> =
    Mutex::new(RefCell::new(Credentials::const_empty()));

/// Latches the default-credentials warning to once per boot.
static DEFAULT_CREDS_WARNED: AtomicBool = AtomicBool::new(false);

/// Replace the stored pair. Called at boot and on every config publish.
pub fn set_credentials(username: &str, password: &str) {
    CREDENTIALS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        guard.username.clear();
        guard.password.clear();
        // `ApiConfig` bounds both fields to `CREDENTIAL_MAX_LEN`.
        let _ = guard.username.push_str(username);
        let _ = guard.password.push_str(password);
    });
}

/// Seed the default pair into an empty store; a populated store is left
/// alone. The listeners seed from `config.api` through
/// [`set_credentials`] instead.
///
/// # Panics
///
/// If a default exceeds `CREDENTIAL_MAX_LEN`, which neither does.
pub fn init_credentials() {
    CREDENTIALS.lock(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.username.is_empty() {
            guard
                .username
                .push_str(DEFAULT_USERNAME)
                .expect("BUG: DEFAULT_USERNAME exceeds CREDENTIAL_MAX_LEN");
            guard
                .password
                .push_str(DEFAULT_PASSWORD)
                .expect("BUG: DEFAULT_PASSWORD exceeds CREDENTIAL_MAX_LEN");
        }
    });
}

/// Warn once per boot while the store holds the default pair. Called
/// after the store is seeded.
pub fn warn_if_default_credentials() {
    // The log write stays outside the lock, which masks interrupts.
    let is_default = CREDENTIALS.lock(|cell| {
        let guard = cell.borrow();
        guard.username.as_str() == DEFAULT_USERNAME && guard.password.as_str() == DEFAULT_PASSWORD
    });
    if is_default && !DEFAULT_CREDS_WARNED.swap(true, Ordering::Relaxed) {
        warn!(
            "default credentials root/root are in use; \
             change them via POST /api/config"
        );
    }
}

/// Snapshot of the current pair.
#[must_use]
pub fn current_credentials() -> Credentials {
    CREDENTIALS.lock(|cell| cell.borrow().clone())
}

/// How a request authenticated. Handlers branch on it: a `Basic` request
/// skips CSRF.
pub enum AuthCtx {
    Basic,
    /// Carries the token for the CSRF check and the logout handler.
    Session(String<32>),
    Anonymous,
}

pub enum AuthError {
    /// Not valid base64, or no `:` separator.
    BadFormat,
    BadCredentials,
}

/// Authenticate from the raw header values: `Authorization: Basic`
/// first, then the `session` cookie, else `Anonymous`. An expired or
/// unknown cookie is anonymous too, so the caller redirects to `/login`.
///
/// # Errors
///
/// `BadFormat` for a malformed header, `BadCredentials` for a mismatch.
///
/// # Panics
///
/// If a validated token exceeds `String<32>`, which cannot happen: only
/// a stored token validates, and stored tokens are 32 characters.
pub fn authenticate(
    authorization: Option<&str>,
    cookie: Option<&str>,
) -> Result<AuthCtx, AuthError> {
    if let Some(authorization_value) = authorization {
        return authenticate_basic(authorization_value);
    }
    if let Some(token) = extract_session_cookie_from_str(cookie)
        && crate::session::validate_session(token)
    {
        let mut owned = String::new();
        owned
            .push_str(token)
            .expect("BUG: validated session token exceeds String<32>");
        return Ok(AuthCtx::Session(owned));
    }
    Ok(AuthCtx::Anonymous)
}

/// Authenticate an `Authorization: Basic <base64>` value. A parse
/// failure still runs the two compares, against the dummies, so the
/// response time does not say which step failed.
fn authenticate_basic(authorization: &str) -> Result<AuthCtx, AuthError> {
    let mut decoded = [0u8; BASIC_AUTH_DECODE_BUFFER_LEN];

    // No early return: the compares below run either way.
    let parse_result: Option<(usize, usize)> = (|| {
        let encoded = authorization.strip_prefix("Basic ")?.trim();
        let length = base64_decode(encoded.as_bytes(), &mut decoded)?;
        let text = core::str::from_utf8(&decoded[..length]).ok()?;
        let colon = text.find(':')?;
        Some((colon, length))
    })();

    let stored = current_credentials();
    let (supplied_username, supplied_password) = match parse_result {
        Some((colon, length)) => {
            let text = core::str::from_utf8(&decoded[..length])
                .expect("BUG: UTF-8 validated in closure above");
            (&text[..colon], &text[colon + 1..])
        }
        None => (DUMMY_USERNAME, DUMMY_PASSWORD),
    };
    let username_ok = constant_time_eq(stored.username.as_str(), supplied_username);
    let password_ok = constant_time_eq(stored.password.as_str(), supplied_password);

    match parse_result {
        None => {
            // Consumed so the optimizer cannot elide the compares.
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

/// Compare two strings in time independent of their contents and
/// lengths: both are padded into `SECRET_MAX_LEN` buffers and xor-ed
/// over the full width, with the length difference folded in after the
/// loop. An input over the bound reads unequal after the same scan, so
/// an oversize secret sharing a prefix with a stored one cannot match.
#[must_use]
pub fn constant_time_eq(left: &str, right: &str) -> bool {
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
    diff |= left.len() ^ right.len();
    !oversize && diff == 0
}

/// The `session` cookie value, or `None`.
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

/// Decode standard base64 into `output`; `None` on an invalid character
/// or truncated input.
fn base64_decode(input: &[u8], output: &mut [u8]) -> Option<usize> {
    /// `=` is rejected here; the caller handles padding, so an `=` outside
    /// the trailing run is a syntax error.
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
    // RFC 4648: at most two trailing `=`, in the final chunk only.
    if padding > 2 {
        return None;
    }
    let decoded_len = chunks * 3 - padding;
    if output.len() < decoded_len {
        return None;
    }
    let mut output_pos = 0;
    let last_chunk_index = chunks.saturating_sub(1);
    for (chunk_index, chunk) in input_stripped.as_chunks::<4>().0.iter().enumerate() {
        let on_last = chunk_index == last_chunk_index;
        let mut bytes = [0u8; 4];
        for (slot_index, &raw) in chunk.iter().enumerate() {
            if raw == b'=' {
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
            #[allow(clippy::cast_possible_truncation)]
            {
                output[output_pos] = (triple >> 16) as u8;
            }
            output_pos += 1;
        }
        if output_pos < decoded_len {
            #[allow(clippy::cast_possible_truncation)]
            {
                output[output_pos] = (triple >> 8) as u8;
            }
            output_pos += 1;
        }
        if output_pos < decoded_len {
            #[allow(clippy::cast_possible_truncation)]
            {
                output[output_pos] = triple as u8;
            }
            output_pos += 1;
        }
    }
    Some(decoded_len)
}
