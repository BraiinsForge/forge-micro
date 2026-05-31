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
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub(crate) static STATE_WATCH: Watch<CSMutex, MiningState, WATCH_CAPACITY> = Watch::new();

/// Module-internal command channel. Single consumer is
/// [`crate::Miner::run`]; producers send via the [`Handle`]
/// inherent methods.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
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
    /// PLL frequency the tuner most recently programmed. Updated
    /// on every `FixedTuner` ramp step so the publisher surfaces the
    /// live operating point, not just the configured target.
    pub frequency: Frequency,
    /// Core voltage the tuner most recently programmed. Updated
    /// on every `FixedTuner` voltage step.
    pub voltage: Voltage,
}

impl MiningState {
    /// Idle baseline used before the first bring-up publishes a
    /// real state. Zero frequency / voltage rather than `None`
    /// because the publisher gauges expose `u32` and `0` is a
    /// visibly-distinct "not-yet-mining" value on a dashboard.
    pub const IDLE: Self = Self {
        running: false,
        chip_count: 0,
        frequency: Frequency::from_khz(0),
        voltage: Voltage::from_mv(0),
    };
}

/// Update the published frequency / voltage without touching the
/// `running` / `chip_count` fields. Called by the tuner after every
/// ramp step so the metrics gauges and any UI subscribers track the
/// live operating point.
pub(crate) fn publish_tuner_step(frequency: Frequency, voltage: Voltage) {
    let mut state = STATE_WATCH.try_get().unwrap_or(MiningState::IDLE);
    state.frequency = frequency;
    state.voltage = voltage;
    STATE_WATCH.sender().send(state);
}

/// Internal command enum carried by the command channel. `pub` so
/// match arms in `crate::miner` can pattern-match every variant;
/// outside crates construct commands via [`Handle`] inherent
/// methods, never by spelling variants directly.
#[derive(Clone)]
pub enum Message {
    Halt,
    Resume,
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

    /// Queue a halt of the work loop. Idempotent.
    pub async fn halt(&self) {
        self.commands.send(Message::Halt).await;
    }

    /// Queue a resume from a previous halt. Idempotent.
    pub async fn resume(&self) {
        self.commands.send(Message::Resume).await;
    }
}
