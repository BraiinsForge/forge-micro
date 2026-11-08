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

//! Lifecycle status and commands for the mining subsystem.
//!
//! Chain frequencies belong to `ubos_metrics::chain`; the shared rail
//! setpoint belongs to `ubos_psu`. Only the miner writes lifecycle status.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};

type CSMutex = CriticalSectionRawMutex;

/// Capacity for lifecycle subscriptions, including the OTA wait.
const WATCH_CAPACITY: usize = 5;

/// Command queue depth. Fire-and-forget callers may overlap; the
/// channel buffers ahead of the work loop and back-pressures
/// `send()` once full.
pub(crate) const CMD_CHANNEL_DEPTH: usize = 4;

/// Module-internal state watch. Its single writer is the miner, through
/// [`Handle::publish`]; readers are obtained via [`Handle::subscribe`].
static STATE_WATCH: Watch<CSMutex, MiningState, WATCH_CAPACITY> = Watch::new();

/// Module-internal command channel. Its single consumer is the miner,
/// through [`Handle::next_message`]; producers send via the [`Handle`]
/// inherent methods.
pub(crate) static COMMAND_CHANNEL: Channel<CSMutex, Message, CMD_CHANNEL_DEPTH> = Channel::new();

/// Module-internal stop request. Its single consumer is the miner,
/// through [`Handle::stopped`]; producers signal it through
/// [`Handle::stop`].
static STOP: Signal<CSMutex, ()> = Signal::new();

/// Mining lifecycle status, separate from hardware readings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiningState {
    /// Chips are held in reset with the rail off until a pool slot
    /// holds a job. Entered at boot and after every rail cut.
    WaitingForPool,
    /// Bring-up is in progress; chips can already be powered.
    Starting,
    /// Every present chain has started receiving work.
    Running,
    /// Chips are held in reset, or no mining hardware is configured.
    Stopped,
}

impl MiningState {
    /// The camelCase wire string for this state used in the REST API
    /// and as the `state` label of the `board_miner_state` metric.
    #[must_use]
    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::WaitingForPool => "waitingForPool",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }

    /// Every state, in lifecycle order. The metrics publisher emits one
    /// series per entry.
    pub(crate) const ALL: [Self; 4] = [
        Self::WaitingForPool,
        Self::Starting,
        Self::Running,
        Self::Stopped,
    ];
}

/// Commands sent through the mining Handle. Outside crates construct
/// them through [`Handle`], never by spelling a variant.
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
        stop: &STOP,
    }
}

/// Consumer-side handle for the mining subsystem.
///
/// `Copy` because it holds only `&'static` references.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, MiningState, WATCH_CAPACITY>,
    commands: &'static Channel<CSMutex, Message, CMD_CHANNEL_DEPTH>,
    stop: &'static Signal<CSMutex, ()>,
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

    /// Request shutdown of the mining state machine from any state. The
    /// miner cuts the rail before [`Miner::run`](crate::Miner::run) returns.
    pub fn stop(&self) {
        self.stop.signal(());
    }

    /// Publish `state` as the miner's lifecycle status, on the watch every
    /// [`Self::state`] and [`Self::subscribe`] reads. Crate-private, so
    /// the miner stays the only writer.
    pub(crate) fn publish(&self, state: MiningState) {
        self.state.sender().send(state);
    }

    /// Wait for a stop request. The miner is the only consumer.
    pub(crate) async fn stopped(&self) {
        self.stop.wait().await;
    }

    /// Take the next halt or resume. The miner is the only consumer.
    pub(crate) async fn next_message(&self) -> Message {
        self.commands.receive().await
    }
}
