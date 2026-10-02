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

//! Mining-pause manager: aggregates pause reasons, owns the
//! `Watch<PauseReasons>` write side, and drives
//! `Handle::halt`/`resume` on the empty/non-empty edge of the
//! pause mask, and `resume` once more on a `Start` that finds the
//! mask empty, for a miner that parked itself. Senders push
//! `Message`s via `Handle::send`;
//! the manager task interprets each message into a `PauseReason` bit
//! transition. The manager task is the sole caller of
//! `Handle::halt`/`resume`.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub(crate) mod manager;
pub(crate) mod message;
pub mod pause;

ubos_events::events! {
    mining_halted_dangerous_temperature()  warn = "Mining halted: dangerous temperature",
    mining_halted_broken_fan()             warn = "Mining halted: broken fan",
    mining_halted_user_request()           warn = "Mining halted: user request",
    mining_halted_ota_update()             warn = "Mining halted: OTA update",
    mining_halted_tuner_fault()            warn = "Mining halted: tuner fault",
    mining_halted_temperature_unreadable() warn = "Mining halted: no temperature reading",
    mining_dangerous_temperature_cleared() info = "Mining halt cleared: dangerous temperature",
    mining_resumed()                       info = "Mining resumed",
}

#[cfg(test)]
#[path = "tests/pause.rs"]
mod tests_pause;

pub use manager::run_manager;
pub use message::Message;
pub use pause::{PauseReason, PauseReasons};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Timer};
use ubos_mining::MiningState;

/// Upper bound on how long [`Handle::stop_mining_and_wait`] waits for the
/// mining loop to confirm it has stopped before giving up. The manager
/// halts mining within one scheduler round-trip; five seconds is well
/// above any observed latency and stops a wedged miner from hanging the
/// OTA request indefinitely.
const MINING_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Subscriber capacity for the pause-reasons watch. No task holds a
/// receiver for long today: the API and the thermal loop read the mask
/// through `pause_reasons`, and a producer that only sends takes no
/// slot. The slots are for a caller of [`Handle::changed`], which holds
/// one for the length of the call, and for the next consumer that
/// follows the mask in its loop. [`Handle::subscribe`] panics past the
/// last one.
const WATCH_CAPACITY: usize = 4;

/// Command queue depth for the manager command channel. A small buffer
/// prevents callers from blocking when the manager task is briefly
/// off-CPU.
const COMMAND_CHANNEL_DEPTH: usize = 8;

type CSMutex = CriticalSectionRawMutex;

/// State watch carrying the live `PauseReasons` mask. Single writer is
/// [`manager::run_manager`].
pub type PauseWatch = Watch<CSMutex, PauseReasons, WATCH_CAPACITY>;

/// Manager command channel. Single consumer is
/// [`manager::run_manager`]; producers send via [`Handle::send`].
pub type CommandChannel = Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH>;

/// The pause mask the firmware's one manager publishes.
pub(crate) static STATE_WATCH: PauseWatch = Watch::new();

/// The commands the firmware's one manager drains.
pub(crate) static COMMAND_CHANNEL: CommandChannel = Channel::new();

/// Per-task subscription to the pause mask, from [`Handle::subscribe`].
pub type PauseSubscription = Receiver<'static, CSMutex, PauseReasons, WATCH_CAPACITY>;

/// Obtain a handle to the manager. `Copy`; callers freely pass it to
/// multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle::new(&STATE_WATCH, &COMMAND_CHANNEL)
}

/// Consumer-side handle for the manager subsystem.
///
/// `Copy` because it holds only `'static` references. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static PauseWatch,
    commands: &'static CommandChannel,
}

impl Handle {
    /// A handle over `state` and `commands`. Firmware calls [`handle`]
    /// instead, which answers for the pair the board runs on. A host
    /// test that drives a manager consumer builds a handle over a pair
    /// it declared itself, and then no other test can observe what it
    /// publishes or what the consumer sends.
    #[must_use]
    pub const fn new(state: &'static PauseWatch, commands: &'static CommandChannel) -> Self {
        Self { state, commands }
    }

    /// Send a `Message` to the manager task. The manager
    /// interprets the message into a `PauseReason` bit transition.
    ///
    /// The send returns when the message is queued, not when the manager
    /// has applied it, so a mask read straight after may still show the
    /// old value. The gap is one manager turn against sample intervals a
    /// thousand times longer, and every transition is idempotent on the
    /// mask, so a sender that misses the gap and asks again changes
    /// nothing.
    pub async fn send(&self, message: Message) {
        self.commands.send(message).await;
    }

    /// Returns the most recent published mask, or a zero mask if
    /// nothing has been published yet (window before the first
    /// thermal tick).
    #[must_use]
    pub fn pause_reasons(&self) -> PauseReasons {
        self.state.try_get().unwrap_or_default()
    }

    /// True while the mask holds `reason`.
    #[must_use]
    pub fn holds(&self, reason: PauseReason) -> bool {
        self.pause_reasons().contains(reason)
    }

    /// Per-task subscription. Call once per consumer task before
    /// entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> PauseSubscription {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Wait for the next mask change and return the new value.
    pub async fn changed(&self) -> PauseReasons {
        let mut subscription = self.subscribe();
        subscription.changed().await
    }

    /// Assert the OTA pause and wait for the miner's stopped state.
    /// Returns `false` if the wait exceeds [`MINING_STOP_TIMEOUT`].
    /// NOTE: The OTA flash driver separately parks the serial workers.
    pub async fn stop_mining_and_wait(&self) -> bool {
        self.send(Message::OtaBegin).await;
        let mining = ubos_mining::handle();
        // Fast path: a `None` state means mining never started, which is
        // already stopped, so there is nothing to await.
        if mining
            .state()
            .is_none_or(|state| state == MiningState::Stopped)
        {
            return true;
        }
        let mut subscription = mining.subscribe();
        let stopped = async { while subscription.changed().await != MiningState::Stopped {} };
        matches!(
            select(stopped, Timer::after(MINING_STOP_TIMEOUT)).await,
            Either::First(()),
        )
    }
}
