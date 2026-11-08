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

//! Cryptographically-secure pseudo-random number generator for session
//! token generation.
//!
//! The CSPRNG is a `ChaCha20Rng` seeded once at boot from the hardware
//! entropy source. Each call to `random_token` draws 24 raw bytes,
//! base64url-encodes them to 32 ASCII characters, and returns the result
//! as a `heapless::String<32>`.
//!
//! The PRNG is held in a `CriticalSectionRawMutex`-guarded `Option` so
//! the static can be const-initialised. `seed_csprng` must be called
//! at firmware startup before any `random_token` call.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use heapless::String;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};

/// Shared CSPRNG instance.
///
/// Seeded from hardware entropy at boot via `seed_csprng`; unset before
/// that call. See `auth::CREDENTIALS` for the rationale on the mutex
/// flavour.
///
/// A blocking mutex, because every span over it is one seed or one
/// 24-byte draw and nothing awaits while it is held. The `RefCell` is
/// what a blocking mutex needs to hand out a `&mut`, and it is reached
/// only from inside the critical section.
static CSPRNG: Mutex<CriticalSectionRawMutex, RefCell<Option<ChaCha20Rng>>> =
    Mutex::new(RefCell::new(None));

/// Seed the CSPRNG from 32 bytes of hardware entropy.
///
/// Each call reseeds the stream; correctness does not depend on the
/// number of calls.
pub fn seed_csprng(seed: [u8; 32]) {
    CSPRNG.lock(|cell| *cell.borrow_mut() = Some(ChaCha20Rng::from_seed(seed)));
}

/// Generate a 32-character URL-safe base64 token for sessions and CSRF.
///
/// Draws 24 bytes from the CSPRNG and encodes them with a URL-safe
/// base64 alphabet (no padding). The resulting string contains only
/// `A`–`Z`, `a`–`z`, `0`–`9`, `-`, and `_`, making it safe in
/// `Set-Cookie` values without quoting.
///
/// Caller MUST have invoked `seed_csprng` first; panics otherwise
/// (firmware bug).
///
/// # Panics
///
/// Panics if `seed_csprng` has not been called before the first call to
/// this function.
pub fn random_token() -> String<32> {
    const URL_SAFE_ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    // Only the draw runs under the lock. The encoding below is pure
    // arithmetic on a local buffer, and holding a critical section across
    // it would mask interrupts for no gain.
    let mut raw = [0u8; 24];
    CSPRNG.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let rng = guard
            .as_mut()
            .expect("BUG: random_token called before seed_csprng");
        rng.fill_bytes(&mut raw);
    });

    // Encode 24 bytes → 32 base64url characters (no padding). Each
    // group of 3 bytes becomes 4 characters; eight groups produce
    // exactly the 32 chars `String<32>` holds, so each push is
    // statically infallible. A single length-check assertion at the
    // bottom localises the contract: a future change to `raw` size,
    // chunk width, or the buffer cap surfaces in one place.
    let mut token: String<32> = String::new();
    for chunk in raw.as_chunks::<3>().0 {
        let triple = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        // Each shift extracts exactly 6 bits; the index into a 64-entry
        // table is always in bounds.
        #[allow(clippy::cast_possible_truncation)]
        {
            // Each push is infallible: 24-byte input yields 8 groups of 3 bytes,
            // producing exactly 32 characters, which matches the String<32> capacity.
            token
                .push(URL_SAFE_ALPHABET[((triple >> 18) & 0x3f) as usize] as char)
                .expect("BUG: token capacity verified at module load");
            token
                .push(URL_SAFE_ALPHABET[((triple >> 12) & 0x3f) as usize] as char)
                .expect("BUG: token capacity verified at module load");
            token
                .push(URL_SAFE_ALPHABET[((triple >> 6) & 0x3f) as usize] as char)
                .expect("BUG: token capacity verified at module load");
            token
                .push(URL_SAFE_ALPHABET[(triple & 0x3f) as usize] as char)
                .expect("BUG: token capacity verified at module load");
        }
    }
    debug_assert_eq!(
        token.len(),
        32,
        "BUG: random_token produced a token of unexpected length"
    );
    token
}
