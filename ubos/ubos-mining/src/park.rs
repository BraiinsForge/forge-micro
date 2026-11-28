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
//! serial workers join the park; the OTA engine, the flash guard of the
//! config store and, on the boards that have one, the firmware-reset task
//! request it.
//!
//! The firmware owns one park as a `static` in `main` and gives it to the
//! serial workers at spawn and to each requester, so that both sides meet at
//! the same counters.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use portable_atomic::{AtomicU32, AtomicUsize, Ordering};

/// One wake receiver for each chain that a board can run.
const WAKE_RECEIVERS: usize = crate::stats::MAX_CHAINS;

/// The receiver a serial worker awaits the park wake on, from
/// [`Park::join`].
pub(crate) type WakeReceiver =
    embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, u32, WAKE_RECEIVERS>;

/// The state of the core-1 park, shared by every serial worker and every
/// requester. It is a type of its own, apart from the serial actor,
/// because the requesters run on the other core and hold no actor.
pub struct Park {
    /// Outstanding park requests. A counter so that nested requesters
    /// compose: OTA holds a park across a whole upload while a config
    /// flash write parks around one erase, and an inner release cannot
    /// drop an outer park.
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
    /// Count of serial actors spinning in the park loop, compared against
    /// [`Park::live`] so a requester waits for every live chain to park.
    parked: AtomicUsize,
    /// Wakes every running worker's `select` when a park is requested, so a
    /// worker blocked in `serial.read` parks without waiting for the next
    /// UART byte. A worker acts on [`Park::requested`], not on the value.
    ///
    /// A `Watch` rather than a `Signal`, which keeps one waker, so two
    /// workers awaiting it would displace each other and only one would
    /// wake. A worker holds its receiver only while `Serial::run` runs.
    wake: Watch<CriticalSectionRawMutex, u32, WAKE_RECEIVERS>,
}

impl Park {
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

    /// Requests that the core-1 serial workers enter their park loop. No
    /// flash operation begins before [`Self::is_parked`] acks, and every
    /// call pairs with exactly one [`Self::release`].
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

    /// Releases one park request, saturating at zero so an unbalanced
    /// release cannot wrap the counter and strand the worker parked
    /// forever.
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
    /// the park loop. [`Self::parked`] is read before [`Self::live`], so an
    /// actor that exits between the two loads can only make this stricter,
    /// never report a live unparked actor as parked.
    #[must_use]
    pub fn is_parked(&self) -> bool {
        let parked = self.parked.load(Ordering::Acquire);
        let live = self.live.load(Ordering::Acquire);
        live > 0 && parked == live
    }

    /// Joins a serial worker to the park and returns the receiver that it
    /// awaits the wake on. From here the worker counts as live, so a
    /// requester waits for it to park. The worker calls [`Self::leave`] on
    /// its way out, and dropping the receiver frees its slot.
    ///
    /// # Panics
    ///
    /// Panics when more workers join than a board has chains.
    pub(crate) fn join(&'static self) -> WakeReceiver {
        let wake = self
            .wake
            .receiver()
            .expect("BUG: park watch must have a receiver slot per chain");
        self.live.fetch_add(1, Ordering::Release);
        wake
    }

    /// Retires a worker that joined. `was_parked` says that it died inside
    /// the park loop, so its parked count is dropped with its live count and
    /// a requester does not wait for it.
    pub(crate) fn leave(&self, was_parked: bool) {
        if was_parked {
            self.parked.fetch_sub(1, Ordering::Release);
        }
        self.live.fetch_sub(1, Ordering::Release);
    }

    /// Returns `true` while a park request is outstanding, which is when a
    /// worker enters the park loop and stays in it.
    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire) > 0
    }

    /// Counts a worker as parked. The worker calls it after it masked its
    /// receive interrupt, because the requester starts the flash operation
    /// once every live worker is counted.
    pub(crate) fn enter(&self) {
        self.parked.fetch_add(1, Ordering::Release);
    }

    /// Counts a worker as running again, after the last request was
    /// released.
    pub(crate) fn exit(&self) {
        self.parked.fetch_sub(1, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn parked_count(&self) -> usize {
        self.parked.load(Ordering::Acquire)
    }
}

impl Default for Park {
    fn default() -> Self {
        Self::new()
    }
}

/// One park request from [`Park::hold`], released on drop, so every exit
/// of the scope releases the park.
pub struct ParkRequest<'a>(&'a Park);

impl Drop for ParkRequest<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}
