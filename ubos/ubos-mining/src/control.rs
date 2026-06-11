//! Public control surface for the mining subsystem.
//!
//! The module owns its primitives as private statics with const
//! constructors. Firmware obtains a [`Handle`] via [`handle`];
//! [`crate::Miner::run`] is the sole writer of `STATE_WATCH` and the
//! sole consumer of `COMMAND_CHANNEL`.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{Receiver, Watch};
use ubos_core::units::{Frequency, Voltage};

#[cfg(feature = "legacy-replay")]
use crate::tuner::replay::trace::Step;

type CSMutex = CriticalSectionRawMutex;

/// Subscriber capacity for the mining state watch. Four receivers
/// covers api, the Step-4 manager, display, and one reference loop
/// without enlarging the watch further.
const WATCH_CAPACITY: usize = 4;

/// Command queue depth. Fire-and-forget callers may overlap; the
/// channel buffers ahead of the work loop and back-pressures
/// `send()` once full.
const CMD_CHANNEL_DEPTH: usize = 4;

/// Module-internal state watch. Single writer is [`crate::Miner::run`];
/// readers are obtained via [`Handle::subscribe`].
pub(crate) static STATE_WATCH: Watch<CSMutex, MiningState, WATCH_CAPACITY> = Watch::new();

/// Module-internal command channel. Single consumer is
/// [`crate::Miner::run`]; producers send via the [`Handle`]
/// inherent methods.
pub(crate) static COMMAND_CHANNEL: Channel<CSMutex, Message, CMD_CHANNEL_DEPTH> = Channel::new();

/// Snapshot of slow-changing operational state. Share counters and
/// hashrate live in `ubos-metrics`; placing them here would wake
/// every subscriber on every share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiningState {
    /// Bring-up completed and the chip is accepting work.
    pub running: bool,
    /// Chips detected on the serial bus during the most recent
    /// bring-up. Zero before the first bring-up completes.
    pub chip_count: u8,
    /// Tuner's intended PLL frequency and rail voltage. `None`
    /// before the first `set_target`.
    pub target: Option<(Frequency, Voltage)>,
}

/// Internal command enum carried by the command channel. `pub` so
/// match arms in `crate::miner` can pattern-match every variant;
/// outside crates construct commands via [`Handle`] inherent
/// methods, never by spelling variants directly.
#[derive(Clone)]
pub enum Message {
    SetTarget {
        freq: Frequency,
        voltage: Voltage,
    },
    Halt,
    Resume,
    #[cfg(feature = "legacy-replay")]
    ExecuteStep(Step),
}

/// Per-task subscription returned by [`Handle::subscribe`].
pub type MiningSubscription = Receiver<'static, CSMutex, MiningState, WATCH_CAPACITY>;

/// Obtain a handle to the mining module. `Copy`; callers freely
/// pass it to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
        commands: &COMMAND_CHANNEL,
    }
}

/// Consumer-side handle for the mining subsystem.
///
/// `Copy` because it holds only `&'static` references.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, MiningState, WATCH_CAPACITY>,
    commands: &'static Channel<CSMutex, Message, CMD_CHANNEL_DEPTH>,
}

impl Handle {
    /// Most recent published state, or `None` before the first
    /// publish.
    #[must_use]
    pub fn state(&self) -> Option<MiningState> {
        self.state.try_get()
    }

    /// Obtain a long-lived subscription. Call once per consumer task
    /// before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    /// The cap `WATCH_CAPACITY` is a compile-time invariant sized to
    /// the consumer set; running out is a programming error.
    #[must_use]
    pub fn subscribe(&self) -> MiningSubscription {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Queue a new tuner target. Applied on the next chip bring-up.
    pub async fn set_target(&self, freq: Frequency, voltage: Voltage) {
        self.commands
            .send(Message::SetTarget { freq, voltage })
            .await;
    }

    /// Queue a halt of the work loop. Idempotent.
    pub async fn halt(&self) {
        self.commands.send(Message::Halt).await;
    }

    /// Queue a resume from a previous halt. Idempotent.
    pub async fn resume(&self) {
        self.commands.send(Message::Resume).await;
    }

    /// Queue a raw bring-up step. Only the legacy `ReplayTuner` of
    /// Step 6 calls this; the gate, the method, and the tuner
    /// delete together one release after Step 6's gates pass.
    #[cfg(feature = "legacy-replay")]
    pub async fn execute_step(&self, step: Step) {
        self.commands.send(Message::ExecuteStep(step)).await;
    }
}
