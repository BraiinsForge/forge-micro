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

//! Shared HTTP middleware: allow-list, authentication, sessions, CSRF, CSPRNG.
//!
//! Consumed by every handler crate, `ubos-webui` and `ubos-api` among
//! them, so the credential store, session table, and CSRF tokens are
//! global to the firmware rather than per-handler.

#![no_std]

pub mod allow_list;
pub mod auth;
pub mod csprng;
pub mod csrf;
pub mod session;

#[cfg(test)]
mod test_time_stubs {
    // `session` and `csrf` call `embassy_time::Instant::now()`.
    // `embassy-time-driver` declares the hook as `extern "Rust"`; no
    // hardware driver is linked into the host test binary, so satisfy
    // the linker with a stub that returns a fixed tick of zero.
    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_now() -> u64 {
        0
    }

    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_schedule_wake(_at: u64, _waker: &core::task::Waker) {
        // No scheduler in the host binary; test paths do not block on
        // timers so leaving this empty is safe.
    }
}
