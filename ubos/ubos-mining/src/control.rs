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
//! Chain frequencies belong to `crate::stats`; the shared rail
//! setpoint belongs to `ubos_psu`. Only the miner writes lifecycle status.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Timer};

use crate::stats::Chains;

type CSMutex = CriticalSectionRawMutex;

/// Capacity for lifecycle subscriptions, including the OTA wait.
const WATCH_CAPACITY: usize = 5;

/// Command queue depth. Fire-and-forget callers may overlap; the
/// channel buffers ahead of the work loop and back-pressures
/// `send()` once full.
pub(crate) const CMD_CHANNEL_DEPTH: usize = 4;

type StateWatch = Watch<CSMutex, MiningState, WATCH_CAPACITY>;

type CommandChannel = Channel<CSMutex, Message, CMD_CHANNEL_DEPTH>;

type StopSignal = Signal<CSMutex, ()>;

static STATE_WATCH: StateWatch = Watch::new();

/// The accounting of every wired chain, as the consumers outside this
/// crate read it. The metrics publisher sends it on its ticker, and the
/// miner sends it when the presence of a chain changes.
static CHAINS_WATCH: Watch<CSMutex, Chains, 0> = Watch::new();

/// Whether the miner holds the rail off because no temperature reading
/// was left. The lifecycle status cannot say it: the miner is `Stopped`
/// for the retry delay, as after any failed bring-up.
static UNREADABLE_WATCH: Watch<CSMutex, bool, 1> = Watch::new();

pub(crate) static COMMAND_CHANNEL: CommandChannel = Channel::new();

static STOP: StopSignal = Signal::new();

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
    /// and as the `state` label of the `miner_state` metric.
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

/// Send the accounting of every wired chain to the readers of
/// [`Handle::chains`].
pub(crate) fn publish_chains() {
    CHAINS_WATCH
        .sender()
        .send(crate::stats::handle().snapshot());
}

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("mining");

/// Consumer-side handle. It holds no data: a firmware takes it once and
/// passes a copy to each task that uses it.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to run the miner.
///
/// It is not `Copy`, so a firmware that gives it to one miner cannot give
/// it to a second.
pub struct Runner(());

impl Handle {
    /// The firmware's one mining handle, and the runner of the miner. A
    /// second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Self) {
        TAKEN.take();
        (Runner(()), Self(()))
    }

    /// Most recent published state, or `None` before the first
    /// publish.
    #[must_use]
    pub fn state(&self) -> Option<MiningState> {
        STATE_WATCH.try_get()
    }

    /// Whether the miner holds the rail off because no temperature reading
    /// was left to watch the machine by. It ends when the miner starts
    /// bring-up again, see "The unreadable temperature" in
    /// `docs/MINING.md`.
    #[must_use]
    pub fn temperature_unreadable(&self) -> bool {
        UNREADABLE_WATCH.try_get().unwrap_or(false)
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
        STATE_WATCH
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// The accounting of every wired chain, as last published. Empty
    /// before the first publish.
    #[must_use]
    pub fn chains(&self) -> Chains {
        CHAINS_WATCH.try_get().unwrap_or_default()
    }

    /// Queue a halt of the work loop. Idempotent.
    pub async fn halt(&self) {
        COMMAND_CHANNEL.send(Message::Halt).await;
    }

    /// Queue a resume from a previous halt. Idempotent.
    pub async fn resume(&self) {
        COMMAND_CHANNEL.send(Message::Resume).await;
    }

    /// Wait until the miner reports `Stopped`, and return `false` if that takes
    /// longer than `timeout`. A miner that never started is already stopped,
    /// so the wait returns at once.
    pub async fn wait_stopped(&self, timeout: Duration) -> bool {
        if self
            .state()
            .is_none_or(|state| state == MiningState::Stopped)
        {
            return true;
        }
        let mut subscription = self.subscribe();
        let stopped = async { while subscription.changed().await != MiningState::Stopped {} };
        matches!(
            select(stopped, Timer::after(timeout)).await,
            Either::First(()),
        )
    }

    /// Request shutdown of the mining state machine from any state. The
    /// miner cuts the rail before [`Miner::run`](crate::Miner::run) returns.
    pub fn stop(&self) {
        STOP.signal(());
    }
}

/// The chain sensor of thermal asks the miner which chains are present.
impl ubos_thermal::PresentChains for Handle {
    fn is_present(&self, chain: ubos_core::index::HashboardIndex) -> bool {
        crate::stats::handle().chain(chain).presence().is_present()
    }
}

/// Each method takes the runner, so only its holder can call it.
#[allow(clippy::unused_self)]
impl Runner {
    /// Publish `state` as the miner's lifecycle status, on the watch every
    /// [`Handle::state`] and [`Handle::subscribe`] read. Crate-private, so
    /// the miner stays the only writer.
    pub(crate) fn publish(&self, state: MiningState) {
        STATE_WATCH.sender().send(state);
    }

    /// Publish whether the rail is off because no temperature reading was
    /// left, for [`Handle::temperature_unreadable`].
    pub(crate) fn publish_temperature_unreadable(&self, unreadable: bool) {
        UNREADABLE_WATCH.sender().send(unreadable);
    }

    /// Wait for a stop request. The future holds nothing of the runner, so
    /// the miner can wait on it while it borrows itself.
    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + use<> {
        STOP.wait()
    }

    /// Wait for a halt and drop every resume before it, because a miner
    /// that is not halted has nothing to resume.
    pub(crate) fn halted(&self) -> impl Future<Output = ()> + use<> {
        async { while !matches!(COMMAND_CHANNEL.receive().await, Message::Halt) {} }
    }

    /// Take the next halt or resume.
    pub(crate) async fn next_message(&self) -> Message {
        COMMAND_CHANNEL.receive().await
    }
}
