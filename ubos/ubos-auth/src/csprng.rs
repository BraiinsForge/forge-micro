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

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use heapless::String;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};

/// Shared CSPRNG instance.
///
/// Seeded from hardware entropy at boot via `seed_csprng`; unset before
/// that call. All `random_token` calls block until the mutex is
/// released by the previous caller. See `auth::CREDENTIALS` for the
/// rationale on the mutex flavour.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
static CSPRNG: Mutex<CriticalSectionRawMutex, Option<ChaCha20Rng>> = Mutex::new(None);

/// Seed the CSPRNG from 32 bytes of hardware entropy.
///
/// Each call reseeds the stream; correctness does not depend on the
/// number of calls.
pub async fn seed_csprng(seed: [u8; 32]) {
    let mut guard = CSPRNG.lock().await;
    *guard = Some(ChaCha20Rng::from_seed(seed));
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
pub async fn random_token() -> String<32> {
    const URL_SAFE_ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut raw = [0u8; 24];
    {
        let mut guard = CSPRNG.lock().await;
        let rng = guard
            .as_mut()
            .expect("BUG: random_token called before seed_csprng");
        rng.fill_bytes(&mut raw);
    }

    // Encode 24 bytes → 32 base64url characters (no padding). Each
    // group of 3 bytes becomes 4 characters; eight groups produce
    // exactly the 32 chars `String<32>` holds, so each push is
    // statically infallible. A single length-check assertion at the
    // bottom localises the contract: a future change to `raw` size,
    // chunk width, or the buffer cap surfaces in one place.
    let mut token: String<32> = String::new();
    for chunk in raw.chunks_exact(3) {
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
