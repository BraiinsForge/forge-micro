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

//! Mining-pause manager: aggregates pause reasons and drives
//! `MiningHandle::halt`/`resume` on the empty/non-empty edge of the
//! pause mask. The manager task is the sole caller of either, and the
//! sole writer of the mask; senders push a [`Message`] and never spell a
//! [`PauseReason`].

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub(crate) mod manager;
pub(crate) mod message;
pub mod pause;

ubos_events::events! {
    mining_halted_broken_fan()             warn = "Mining halted: broken fan",
    mining_halted_user_request()           warn = "Mining halted: user request",
    mining_halted_ota_update()             warn = "Mining halted: OTA update",
    mining_halted_tuner_fault()            warn = "Mining halted: tuner fault",
    mining_resumed()                       info = "Mining resumed",
}

#[cfg(test)]
#[path = "tests/pause.rs"]
mod tests_pause;

pub use manager::run_manager;
pub use message::Message;
pub use pause::{PauseReason, PauseReasons};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{Receiver, Sender, Watch};

/// Subscriber capacity for the pause-reasons watch. A reader that only
/// calls `pause_reasons`, and a producer that only sends, take no slot;
/// the slots are for a caller of [`Handle::changed`], which holds one for
/// the length of the call. [`Handle::subscribe`] panics past the last.
const WATCH_CAPACITY: usize = 4;

/// Command queue depth. The buffer keeps a caller from blocking while the
/// manager task is briefly off-CPU.
const COMMAND_CHANNEL_DEPTH: usize = 8;

type CSMutex = CriticalSectionRawMutex;

/// Single writer is [`manager::run_manager`].
type PauseWatch = Watch<CSMutex, PauseReasons, WATCH_CAPACITY>;

/// Single consumer is [`manager::run_manager`]; producers send via
/// [`Handle::send`].
type CommandChannel = Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH>;

static STATE_WATCH: PauseWatch = Watch::new();

static COMMAND_CHANNEL: CommandChannel = Channel::new();

/// Per-task subscription to the pause mask, from [`Handle::subscribe`].
pub type PauseSubscription = Receiver<'static, CSMutex, PauseReasons, WATCH_CAPACITY>;

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("manager");

/// Consumer-side handle. It holds no data: a firmware takes it once and
/// passes a copy to each task that uses it.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to run the task that routes the commands.
///
/// It is not `Copy`, so a firmware that gives it to one task cannot give
/// it to a second.
pub struct Runner(());

#[allow(clippy::unused_self)]
impl Runner {
    pub(crate) fn publisher(&self) -> Sender<'static, CSMutex, PauseReasons, WATCH_CAPACITY> {
        STATE_WATCH.sender()
    }

    pub(crate) async fn next_message(&self) -> Message {
        COMMAND_CHANNEL.receive().await
    }
}

impl Handle {
    /// The firmware's one manager handle, and the runner of the task that
    /// routes the commands. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Self) {
        TAKEN.take();
        (Runner(()), Self(()))
    }

    /// The send returns when the message is queued, not when the manager
    /// has applied it, so a mask read straight after may still show the
    /// old value. Every transition is idempotent on the mask, so a sender
    /// that misses the gap and asks again changes nothing.
    pub async fn send(&self, message: Message) {
        COMMAND_CHANNEL.send(message).await;
    }

    /// The most recent published mask, or a zero mask in the window
    /// before the manager task starts.
    #[must_use]
    pub fn pause_reasons(&self) -> PauseReasons {
        STATE_WATCH.try_get().unwrap_or_default()
    }

    /// True while the mask holds `reason`.
    #[must_use]
    pub fn holds(&self, reason: PauseReason) -> bool {
        self.pause_reasons().contains(reason)
    }

    /// Call once per consumer task, before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> PauseSubscription {
        STATE_WATCH
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    pub async fn changed(&self) -> PauseReasons {
        let mut subscription = self.subscribe();
        subscription.changed().await
    }
}
