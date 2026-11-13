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

//! The token CSPRNG: a `ChaCha20Rng` the firmware seeds from hardware
//! entropy before the first `random_token`.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use heapless::String;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};

/// `None` until [`seed_csprng`] runs.
static CSPRNG: Mutex<CriticalSectionRawMutex, RefCell<Option<ChaCha20Rng>>> =
    Mutex::new(RefCell::new(None));

/// Seed, or reseed, from 32 bytes of hardware entropy.
pub fn seed_csprng(seed: [u8; 32]) {
    CSPRNG.lock(|cell| *cell.borrow_mut() = Some(ChaCha20Rng::from_seed(seed)));
}

/// A 32-character URL-safe base64 token, 24 random bytes unpadded, so
/// it needs no quoting in `Set-Cookie`.
///
/// # Panics
///
/// Before [`seed_csprng`] has run.
pub fn random_token() -> String<32> {
    const URL_SAFE_ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    // Only the draw runs under the lock, which masks interrupts.
    let mut raw = [0u8; 24];
    CSPRNG.lock(|cell| {
        let mut guard = cell.borrow_mut();
        let rng = guard
            .as_mut()
            .expect("BUG: random_token called before seed_csprng");
        rng.fill_bytes(&mut raw);
    });

    // Eight groups of three bytes make the 32 characters exactly; the
    // assertion at the bottom is where a size change surfaces.
    let mut token: String<32> = String::new();
    for chunk in raw.as_chunks::<3>().0 {
        let triple = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        #[allow(clippy::cast_possible_truncation)]
        {
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
