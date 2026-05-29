//! Mining-pause manager: aggregates pause reasons, owns the
//! `Watch<PauseReasons>` write side, and drives
//! `Handle::halt`/`resume` on the empty/non-empty edge of the
//! pause mask. Senders push `Message`s via `Handle::send`;
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

#[cfg(test)]
#[path = "tests/pause.rs"]
mod tests_pause;

pub use manager::run_manager;
pub use message::Message;
pub use pause::{PauseReason, PauseReasons};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{Receiver, Watch};

/// Subscriber capacity for the pause-reasons watch. Four receivers
/// covers API, display, and two future consumers without widening
/// the watch further.
const WATCH_CAPACITY: usize = 4;

/// Command queue depth for the manager command channel. A small buffer
/// prevents callers from blocking when the manager task is briefly
/// off-CPU.
const COMMAND_CHANNEL_DEPTH: usize = 8;

type CSMutex = CriticalSectionRawMutex;

/// Module-internal state watch carrying the live `PauseReasons` mask.
/// Single writer is [`manager::run_manager`].
pub(crate) static STATE_WATCH: Watch<CSMutex, PauseReasons, WATCH_CAPACITY> = Watch::new();

/// Module-internal manager command channel. Single consumer is
/// [`manager::run_manager`]; producers send via [`Handle::send`].
pub(crate) static COMMAND_CHANNEL: Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH> =
    Channel::new();

/// Obtain a handle to the manager. `Copy`; callers freely pass it to
/// multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
        commands: &COMMAND_CHANNEL,
    }
}

/// Consumer-side handle for the manager subsystem.
///
/// `Copy` because it holds only `'static` references. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, PauseReasons, WATCH_CAPACITY>,
    commands: &'static Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH>,
}

impl Handle {
    /// Send a `Message` to the manager task. The manager
    /// interprets the message into a `PauseReason` bit transition.
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

    /// Per-task subscription. Call once per consumer task before
    /// entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<'static, CSMutex, PauseReasons, WATCH_CAPACITY> {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Wait for the next mask change and return the new value.
    pub async fn changed(&self) -> PauseReasons {
        let mut subscription = self.subscribe();
        subscription.changed().await
    }
}
