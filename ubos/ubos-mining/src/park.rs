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

//! The core-1 park, a cooperative pause of the serial workers.
//!
//! A flash operation on one core stalls fetches from flash on the other, so
//! the core that runs the serial workers must stand still while it lasts. The
//! request methods are called from the OTA flash path and, on the boards that
//! have one, from the firmware-reset path; a board that exercises neither
//! leaves the park loop dormant for the image's lifetime.
//!
//! The firmware owns one park as a `static` in `main` and gives it to the
//! serial workers at spawn and to each requester, so that both sides meet at
//! the same counters.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use portable_atomic::{AtomicU32, AtomicUsize, Ordering};

/// One wake receiver for each chain that a board can run.
const WAKE_RECEIVERS: usize = ubos_metrics::chain::MAX_CHAINS;

/// The state of the core-1 park, shared by every serial worker and every
/// requester.
///
/// The firmware owns one park as a `static` and hands it to every task that
/// holds it. The serial workers join it. The OTA engine, the flash guard of
/// the config store and, on the boards that have one, the firmware-reset
/// task request it. All of them meet at the same counters. It is a type of
/// its own, apart from the serial actor, because the requesters run on the
/// other core and hold no actor.
pub struct Park {
    /// Outstanding park requests. The serial worker enters the park loop
    /// and stays in it while this is non-zero.
    ///
    /// It is a counter so that nested requesters compose: OTA holds
    /// a park across a whole upload while a config flash write parks around
    /// its single erase/write. [`Park::request`] increments,
    /// [`Park::release`] decrements, and the worker parks while the
    /// count is non-zero, so an inner release cannot drop an outer park.
    requested: AtomicU32,
    /// Count of serial actors currently running `Serial::run` on any core,
    /// across every chain a board owns.
    ///
    /// A flash write that lands when this is `0` has nothing to park, and
    /// waiting for an ack that can never come would fail the write. The case
    /// that matters is a worker that has exited on serial EOF: its task then
    /// idles forever, so every later config save would burn the writer's whole
    /// park budget and fail. `run` masks the chip-UART RX interrupt on its way
    /// out, so once an actor's count is dropped there is no interrupt left to
    /// wake its core out of the flash-pause `wfe` either.
    ///
    /// The count is `0` for a short window at boot too, between a core arming
    /// the UART interrupt and reaching `run`. Nothing writes flash there:
    /// config saves come from the API, which needs the network up.
    live: AtomicUsize,
    /// Count of serial actors currently spinning in `Serial::park_if_requested`'s
    /// park loop.
    ///
    /// [`Park::is_parked`] compares this against [`Park::live`] so a park
    /// requester can wait for every live chain to park, not just one.
    parked: AtomicUsize,
    /// Wakes every running serial worker's `select` loop when a park is
    /// requested.
    ///
    /// A worker may be blocked inside `serial.read` waiting for UART data
    /// when a park request arrives. A send here wakes every worker's
    /// `select4` so the park arm fires promptly rather than waiting for the
    /// next UART byte. The value carried is the outstanding request count at
    /// the time of the send; a worker acts on [`Park::requested`] itself, so
    /// what the send is for is the wake rather than the value.
    ///
    /// It is a `Watch` because a board drives one worker per chain and the
    /// park is addressed to all of them. A `Signal` keeps a single waker, so
    /// two workers awaiting one would displace each other's waker on every
    /// poll and only one of them would be woken. A `Watch` keeps a waker per
    /// receiver, and a worker takes its receiver in `Serial::run` and drops
    /// it on the way out, so a chain whose task never started or has already
    /// exited holds none and a send still reaches every worker that is
    /// running. The field is private, so a requester parks through
    /// [`Park::request`] and the methods beside it.
    wake: Watch<CriticalSectionRawMutex, u32, WAKE_RECEIVERS>,
}

impl Park {
    /// A park that no one has requested and no worker has joined. It is
    /// `const` so that a firmware holds its one park in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            requested: AtomicU32::new(0),
            live: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            wake: Watch::new(),
        }
    }

    /// Requests that the core-1 serial workers enter their park loop.
    ///
    /// Increments [`Self::requested`] with `Release` ordering and sends on
    /// [`Self::wake`] to wake every running worker's `select` loop
    /// immediately. After this call, poll [`Self::is_parked`] to wait for
    /// the ack; flash operations must not begin until the ack is observed.
    /// Pair every call with exactly one [`Self::release`].
    pub fn request(&self) {
        let requested = self.requested.fetch_add(1, Ordering::Release) + 1;
        self.wake.sender().send(requested);
    }

    /// Requests a park that is released when the returned value is dropped,
    /// for a requester whose park spans one scope.
    #[must_use = "the park is released when this is dropped"]
    pub fn hold(&self) -> ParkRequest<'_> {
        self.request();
        ParkRequest(self)
    }

    /// Releases one core-1 park request.
    ///
    /// Decrements [`Self::requested`] with `Release` ordering, saturating at
    /// zero so an unbalanced release cannot wrap the counter and strand the
    /// worker parked forever. The worker leaves its busy-spin only once the
    /// count reaches zero, so an inner requester's release does not drop an
    /// outer requester's park.
    pub fn release(&self) {
        let _ = self
            .requested
            .fetch_update(Ordering::Release, Ordering::Acquire, |count| {
                Some(count.saturating_sub(1))
            });
    }

    /// Re-wakes every serial worker for a park that is already requested.
    ///
    /// Sends on [`Self::wake`] without touching [`Self::requested`], so a
    /// requester polling for the ack can retry a cross-core wake that was lost
    /// while core 1 sat in `wfe` with interrupts briefly disabled. Calling
    /// [`Self::request`] again instead would leave an increment behind
    /// that no release balances.
    pub fn resignal(&self) {
        self.wake
            .sender()
            .send(self.requested.load(Ordering::Acquire));
    }

    /// Returns `true` while at least one chain's serial worker is running and
    /// able to answer a park request.
    ///
    /// A flash writer must check this before waiting on [`Self::is_parked`]:
    /// when it returns `false` there is no worker to park, and no chip-UART
    /// interrupt armed on any core either, so the write is safe to run
    /// unparked. See [`Self::live`].
    #[must_use]
    pub fn has_workers(&self) -> bool {
        self.live.load(Ordering::Acquire) > 0
    }

    /// Returns `true` while every live chain's serial worker is spinning in
    /// the park loop.
    ///
    /// Reads [`Self::parked`] before [`Self::live`]. An actor that dies
    /// between the two loads, when its `run` returns and decrements both
    /// counts, can only make this check stricter, never looser: the parked count read
    /// first can undercount relative to the live count read second, so a
    /// borderline case reports not-yet-parked rather than falsely reporting
    /// every actor parked while a live, unparked actor still runs. A dead
    /// actor must never be allowed to block a flash write by appearing to
    /// still need parking. Flash operations may proceed once this returns
    /// `true`; no worker leaves the park loop until [`Self::release`]
    /// is called.
    #[must_use]
    pub fn is_parked(&self) -> bool {
        let parked = self.parked.load(Ordering::Acquire);
        let live = self.live.load(Ordering::Acquire);
        live > 0 && parked == live
    }
}

impl Default for Park {
    fn default() -> Self {
        Self::new()
    }
}

/// One park request from [`Park::hold`], which the drop releases. It exists
/// so that every exit of the scope releases the park.
pub struct ParkRequest<'a>(&'a Park);

impl Drop for ParkRequest<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}
