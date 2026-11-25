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

//! The expiry of a measured metric: when it was taken and how long it
//! stays in the exposition. A constant has no expiry, so a metric holds
//! an `Option<Expiry>` and the two states cannot mix.

use core::num::NonZeroU32;

use embassy_time::Duration;

#[cfg(test)]
#[path = "tests/freshness.rs"]
mod tests;

const MILLIS_PER_SECOND: u64 = 1_000;

/// The time of a measurement in monotonic milliseconds and the seconds
/// it stays valid. The render reads it to omit a series whose publisher
/// stopped pushing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Expiry {
    at_ms: u64,
    ttl_secs: NonZeroU32,
}

/// Each slot of the map and of the channel holds an `Option<Expiry>`, so
/// its size is a RAM cost that a board pays many times over.
const _: () = assert!(
    size_of::<Option<Expiry>>() == 16,
    "Option<Expiry> no longer fits 16 bytes"
);

impl Expiry {
    /// An expiry for a measurement taken at `at_ms`. The TTL rounds up to
    /// whole seconds and is held between one second and `u32::MAX`
    /// seconds, so the zero value is never stored.
    pub(crate) fn new(at_ms: u64, ttl: Duration) -> Self {
        let secs = ttl.as_millis().div_ceil(MILLIS_PER_SECOND);
        let secs = u32::try_from(secs).unwrap_or(u32::MAX);
        Self {
            at_ms,
            ttl_secs: NonZeroU32::new(secs).unwrap_or(NonZeroU32::MIN),
        }
    }

    /// The time of the measurement in monotonic milliseconds.
    pub(crate) fn at_ms(self) -> u64 {
        self.at_ms
    }

    /// The seconds the measurement stays valid.
    #[cfg(test)]
    pub(crate) fn ttl_secs(self) -> NonZeroU32 {
        self.ttl_secs
    }
}
