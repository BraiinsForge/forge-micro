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

//! Single-task actor for the ASIC chain serial line.
//!
//! One task owns the full-duplex serial line and the in-flight work
//! registry; outside callers go through [`Serial`]'s typed methods to
//! reset the line, bring the chain up, retune the frequency, or gate work
//! dispatch. The actor is generic over the concrete [`ChainDriver`] the
//! caller injects and implements [`Transport`] over the UART, so the
//! vendor-specific framing and bring-up recipe live in `ubos-asic` or in
//! a vendor's own crate against that same seam. The actor holds a work
//! receiver for every pool slot of a
//! `&'static AsicSession<N>`; on each work tick a
//! [`crate::scheduler::Scheduler`] picks which slot's cell to dispatch
//! from, restricted to the slots that currently hold work, and every
//! diff-1 chip share feeds back into it so the delivered split tracks
//! the scheduler's configured policy. Found shares submit back to the
//! slot whose dispatch produced them via `AsicSession::submit_solution`.
//!
//! The core-1 park surface at the end of this module is the exception to
//! all of that, and it is free functions on purpose. Those five reach
//! cross-core state in private statics, because every caller is on the
//! other core from the actor and holds no [`Serial`]: `ubos-ota` parks
//! core 1 before a flash write, and a board's `main` parks it at boot. A
//! static is the only thing that crosses from core 0 to core 1, so a
//! method on the handle could not serve them. The park is addressed to
//! every chain at once rather than to one actor, so the wake is a
//! [`embassy_sync::watch::Watch`] with a receiver per actor rather than
//! a `Signal`, which keeps one waker and so would wake one actor only.

use core::fmt;
use core::ops::ControlFlow;

use embassy_futures::select::{Either4, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{DynReceiver, Watch};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use embedded_io::ErrorKind;
use embedded_io_async::{Error as _, Read, Write};
use mining_protocol::hashing::hash_meets_target;
use portable_atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use serial_baudrate::BaudRate;
use stratum_client::{AsicSession, Solution, WorkAssignment};
use ubos_asic::{
    BaudRateError, ChainDriver, ChainSolution, ChipError, HashboardDescriptor, JobVersion,
    PollProgress, ResultMode, Transport, WorkJob, solution_hash,
};
use ubos_core::units::Frequency;
use ubos_logging::{Debug2Format, error, info, trace, warn};

use ubos_metrics::chain::ChainStats;

#[cfg(test)]
#[path = "tests/serial.rs"]
mod tests;

// ---------------------------------------------------------------------------
// Core-1 park primitives
//
// These three items coordinate a cooperative pause of the serial worker
// so a different core can run flash operations safely. The request
// functions are called from the OTA flash path and, on the boards that
// have one, from the firmware-reset path; a board that exercises
// neither leaves the park loop dormant for the image's lifetime.
// ---------------------------------------------------------------------------

/// Outstanding park requests. Non-zero means the serial worker should
/// enter (and stay in) the park loop.
///
/// A counter rather than a flag so nested requesters compose: OTA holds
/// a park across a whole upload while a config flash write parks around
/// its single erase/write. [`request_core1_park`] increments,
/// [`release_core1_park`] decrements, and the worker parks while the
/// count is non-zero, so an inner release cannot drop an outer park.
pub(crate) static PARK_REQUESTED: AtomicU32 = AtomicU32::new(0);

/// Count of serial actors currently running [`Serial::run`] on any core,
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
static LIVE_ACTORS: AtomicUsize = AtomicUsize::new(0);

/// Count of serial actors currently spinning in [`Serial::park_if_requested`]'s
/// park loop.
///
/// [`core1_is_parked`] compares this against [`LIVE_ACTORS`] so a park
/// requester can wait for every live chain to park, not just one.
static PARKED_ACTORS: AtomicUsize = AtomicUsize::new(0);

/// Wakes every running serial worker's `select` loop when a park is
/// requested.
///
/// A worker may be blocked inside `serial.read` waiting for UART data
/// when a park request arrives. A send here wakes every worker's
/// `select4` so the park arm fires promptly rather than waiting for the
/// next UART byte. The value carried is the outstanding request count at
/// the time of the send; a worker acts on [`PARK_REQUESTED`] itself, so
/// what the send is for is the wake rather than the value.
///
/// A `Watch` rather than a `Signal` because a board drives one worker
/// per chain and the park is addressed to all of them. A `Signal` keeps
/// a single waker, so two workers awaiting one would displace each
/// other's waker on every poll and only one of them would be woken. A
/// `Watch` keeps a waker per receiver, and a worker takes its receiver
/// in [`Serial::run`] and drops it on the way out, so a chain whose task
/// never started or has already exited holds none and a send still
/// reaches every worker that is running. Module-private: external
/// callers park through [`request_core1_park`] and friends, never by
/// touching the watch directly.
static PARK_WATCH: Watch<CriticalSectionRawMutex, u32, { ubos_metrics::chain::MAX_CHAINS }> =
    Watch::new();

/// Requests that the core-1 serial workers enter their park loop.
///
/// Increments [`PARK_REQUESTED`] with `Release` ordering and sends on
/// [`PARK_WATCH`] to wake every running worker's `select` loop
/// immediately. After this call, poll [`core1_is_parked`] to wait for
/// the ack; flash operations must not begin until the ack is observed.
/// Pair every call with exactly one [`release_core1_park`].
pub fn request_core1_park() {
    let requested = PARK_REQUESTED.fetch_add(1, Ordering::Release) + 1;
    PARK_WATCH.sender().send(requested);
}

/// Releases one core-1 park request.
///
/// Decrements [`PARK_REQUESTED`] with `Release` ordering, saturating at
/// zero so an unbalanced release cannot wrap the counter and strand the
/// worker parked forever. The worker leaves its busy-spin only once the
/// count reaches zero, so an inner requester's release does not drop an
/// outer requester's park.
pub fn release_core1_park() {
    let _ = PARK_REQUESTED.fetch_update(Ordering::Release, Ordering::Acquire, |count| {
        Some(count.saturating_sub(1))
    });
}

/// Re-wakes every serial worker for a park that is already requested.
///
/// Sends on [`PARK_WATCH`] without touching [`PARK_REQUESTED`], so a
/// requester polling for the ack can retry a cross-core wake that was lost
/// while core 1 sat in `wfe` with interrupts briefly disabled. Calling
/// [`request_core1_park`] again instead would leave an increment behind
/// that no release balances.
pub fn signal_core1_park() {
    PARK_WATCH
        .sender()
        .send(PARK_REQUESTED.load(Ordering::Acquire));
}

/// Returns `true` while at least one chain's serial worker is running and
/// able to answer a park request.
///
/// A flash writer must check this before waiting on [`core1_is_parked`]:
/// when it returns `false` there is no worker to park, and no chip-UART
/// interrupt armed on any core either, so the write is safe to run
/// unparked. See [`LIVE_ACTORS`].
#[must_use]
pub fn core1_serial_running() -> bool {
    LIVE_ACTORS.load(Ordering::Acquire) > 0
}

/// Returns `true` while every live chain's serial worker is spinning in
/// the park loop.
///
/// Reads [`PARKED_ACTORS`] before [`LIVE_ACTORS`]. An actor that dies
/// between the two loads (its `run` returns and decrements both counts)
/// can only make this check stricter, never looser: the parked count read
/// first can undercount relative to the live count read second, so a
/// borderline case reports not-yet-parked rather than falsely reporting
/// every actor parked while a live, unparked actor still runs. A dead
/// actor must never be allowed to block a flash write by appearing to
/// still need parking. Flash operations may proceed once this returns
/// `true`; no worker leaves the park loop until [`release_core1_park`]
/// is called.
#[must_use]
pub fn core1_is_parked() -> bool {
    let parked = PARKED_ACTORS.load(Ordering::Acquire);
    let live = LIVE_ACTORS.load(Ordering::Acquire);
    live > 0 && parked == live
}

use crate::registry::WorkRegistry;
use crate::scheduler::Scheduler;

/// Polling time between status turns on a poll chip.
///
/// The chain's own sensors are a board's only thermometer on some
/// families, so the readings cannot be dropped, but they share the wire
/// with work. One turn per this much polling is every eighth poll on a
/// chip polled at sixty milliseconds and every poll on one polled at
/// this period or slower; on a long chain the sweep itself outlasts the
/// poll period, and the turn lands once per sweep.
const STATUS_TURN_PERIOD: Duration = Duration::from_millis(480);

/// The steady state of a poll family: the ticker between two reads of
/// one chip, and how many of those reads are left before the next
/// status turn. One object because the reload holds them together: a
/// countdown that reaches zero takes the turn and starts again at the
/// count the period implies.
struct PollCadence {
    ticker: Ticker,
    polls_per_turn: u8,
    countdown: u8,
    progress: PollProgress,
}

impl PollCadence {
    /// The cadence a family's stated poll period implies.
    ///
    /// # Panics
    ///
    /// Panics on a zero period, which would divide by zero below and is
    /// a driver bug rather than a runtime condition.
    fn every(period: Duration) -> Self {
        assert!(
            period.as_ticks() > 0,
            "BUG: poll family states a zero period"
        );
        // A family polled at or slower than the status period takes a
        // turn on every poll.
        let polls_per_turn = u8::try_from(STATUS_TURN_PERIOD.as_ticks() / period.as_ticks())
            .unwrap_or(u8::MAX)
            .max(1);
        Self {
            ticker: Ticker::every(period),
            polls_per_turn,
            countdown: polls_per_turn,
            progress: PollProgress::Complete,
        }
    }

    /// Wait for the next poll, and say whether it is the one that takes
    /// a status turn as well.
    async fn next(&mut self) -> bool {
        if self.progress == PollProgress::More {
            return false;
        }
        self.ticker.next().await;
        // Skip missed ticks after a slow sweep instead of catching up.
        self.ticker.reset();
        self.progress = PollProgress::More;
        self.countdown -= 1;
        if self.countdown > 0 {
            return false;
        }
        self.countdown = self.polls_per_turn;
        true
    }
}

/// Scratch buffer size for a single UART read on the push path. Pinned
/// to the seam's ingest contract so one full read can never decode into
/// more solutions than a batch carries.
const READ_CHUNK: usize = ubos_asic::MAX_INGEST_CHUNK;

/// Hex pretty-printer for trace logs. Wraps a byte slice and formats it
/// as `"AA BB CC"` under both `core::fmt::Display` and `defmt::Format`,
/// so the same `trace!("...{}...", HexBytes(...))` call site routes
/// correctly under either logging backend.
pub struct HexBytes<'a>(pub &'a [u8]);

impl fmt::Display for HexBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for HexBytes<'_> {
    fn format(&self, f: defmt::Formatter) {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                defmt::write!(f, " ");
            }
            defmt::write!(f, "{:02X}", b);
        }
    }
}

/// Hex pretty printer that prints raw little-endian representation of block hash as a big-endian hex string that we commonly see
/// in bitcoin industry.
pub struct BlockHash<'a>(pub &'a [u8]);

impl fmt::Display for BlockHash<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0.iter().rev() {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for BlockHash<'_> {
    fn format(&self, f: defmt::Formatter) {
        for b in self.0.iter().rev() {
            defmt::write!(f, "{:02x}", b);
        }
    }
}

/// Outbound command from a [`Serial`] handle to the server task. Replies
/// come back through [`Serial::reply`].
#[derive(Debug, Clone)]
pub enum SerialMessage {
    /// Re-arm the line at the chip's boot baud and clear pending work and
    /// driver state.
    Reset,
    /// Reset and enumerate the chain; the reply carries the chip count.
    ResetEnumerate,
    /// Configure the enumerated chain to its safe-boot operating point,
    /// switching to the fast UART.
    Configure,
    /// Read every chip's temperature and core voltage once, for the
    /// first reading after enumeration. The reply fails when no chip
    /// answered.
    PvtSweep,
    /// Program the hash-clock frequency.
    SetFrequency(Frequency),
    /// Begin dispatching work at `period`. The ticker's first tick, and
    /// so the first dispatch, comes one full period after the command.
    StartSendingWork(Duration),
    /// Stop dispatching work. On a push chip, in-flight responses still
    /// arrive through the always-armed read; a poll chip's ticker stops
    /// with the work, so results still latched in its nonce FIFO are not
    /// collected until work restarts.
    StopSendingWork,
    /// Tell the server task to exit its loop.
    Terminate,
}

/// Reply from the server task to a [`Serial`] caller. The caller mutex
/// serialises issuers, so the single reply slot never interleaves.
#[derive(Debug, Clone, Copy)]
enum Reply {
    /// Result of a command with no payload.
    Unit(Result<(), ErrorKind>),
    /// Enumerated chip count.
    Count(u8),
    /// Result of a bring-up or frequency-programming command.
    Freq(Result<(), ChipError>),
}

/// Depth of the command channel feeding the serial actor. A const
/// generic on the channel field, so it cannot be an associated const of
/// `Serial`.
const CMD_QUEUE_DEPTH: usize = 16;

/// The chip-side serial actor.
///
/// Construct as a `static`: every primitive inside is `const`-initialisable.
/// Callers send commands through the typed methods; the server task is
/// driven by [`Self::run`] on the same instance.
pub struct Serial {
    cmd: Channel<CriticalSectionRawMutex, (u32, SerialMessage), CMD_QUEUE_DEPTH>,
    /// Reply slot for the most recently answered command, tagged with the
    /// command's sequence number. The caller mutex serialises issuers so
    /// two callers cannot race on this single signal; the tag lets a
    /// caller discard the orphaned reply of a command whose issuer was
    /// cancelled between send and wait.
    reply: Signal<CriticalSectionRawMutex, (u32, Reply)>,
    /// Serialises caller-side `cmd.send + reply.wait` pairs so their reply
    /// slots never interleave, and carries the next command sequence
    /// number. `CriticalSectionRawMutex` rather than `NoopRawMutex` so
    /// `Serial` is `Sync` and can live as a workspace `static`.
    caller: Mutex<CriticalSectionRawMutex, u32>,
    /// This actor's index among the machine's hashboards, used to
    /// partition a pool-granted version-rolling mask so sibling chains
    /// never hash identical headers, and to index into
    /// `ubos_metrics::chain`.
    chain: usize,
    /// Total hashboard count this machine has wired up, the other half
    /// of the version-mask partition.
    chains: usize,
    /// Latest pool dispatch weights, which the actor applies to its
    /// scheduler on its next pass through the loop.
    ///
    /// A `Watch` rather than a command, because the publisher is the
    /// pool-weights watcher and a command would make it wait behind a
    /// bring-up on the caller mutex, which runs for seconds. `send` takes
    /// a critical section and no await, so the watcher never blocks; the
    /// whole array moves under that one lock, so the actor cannot read a
    /// half-written set. One receiver, which the actor takes in
    /// [`Serial::run`].
    pool_weights: Watch<CriticalSectionRawMutex, [u32; crate::consts::MAX_POOL_SLOTS], 1>,
    /// `true` while this actor is parked. Set and cleared by
    /// [`Serial::park_if_requested`]; read back by [`Serial::stand_down`]
    /// so an actor that dies mid-park still releases its
    /// [`PARKED_ACTORS`] count.
    parked: AtomicBool,
}

/// [`Transport`] over the actor's UART. Frames written by the driver go
/// straight to the line; reads are bounded by the driver-supplied
/// timeout; the baud switch programs the driver-supplied rate verbatim —
/// each driver's host-rate hook already names a rate the host controller
/// can achieve, so no vendor rate mapping lives here.
struct SerialIo<'a, S> {
    serial: &'a mut S,
}

impl<S> Transport for SerialIo<'_, S>
where
    S: Read + Write + BaudRate,
{
    async fn write_all(&mut self, bytes: &[u8]) {
        trace!("TX ({} bytes): {}", bytes.len(), HexBytes(bytes));
        if let Err(e) = self.serial.write_all(bytes).await {
            error!("Serial write error: {:?}", Debug2Format(&e));
        }
    }

    async fn flush(&mut self) {
        if let Err(e) = self.serial.flush().await {
            error!("Serial flush error: {:?}", Debug2Format(&e));
        }
    }

    async fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> usize {
        // A read error is consumed and the read continues. Serial
        // drivers queue line errors in order with the data, so a break
        // or framing error from an earlier line event surfaces on
        // whichever later read pops it, with the bytes the caller wants
        // behind it. Ending the read on the popped error made a chain
        // look mute while it was answering: the error ate the attempt
        // and the next drain ate the answer.
        //
        // The consume-and-continue cannot spin on a stuck fault on this
        // transport: `BufferedUart` reports one sticky RX error per read
        // that returns it and masks further RX interrupts until that read
        // drains it, so at most one extra iteration runs per real line
        // event. A backend without that interlock — a future DMA or
        // hardware-FIFO RX path — would need its own backoff added here.
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return 0;
            };
            match with_timeout(remaining, self.serial.read(buf)).await {
                Ok(Ok(n)) => {
                    trace!("RX ({} bytes): {}", n, HexBytes(&buf[..n]));
                    return n;
                }
                Ok(Err(e)) => {
                    warn!("Serial read error: {:?}", Debug2Format(&e));
                }
                Err(_) => return 0,
            }
        }
    }

    fn set_baud(&mut self, hz: u32) -> Result<(), BaudRateError> {
        if hz == 0 {
            error!("Refusing zero baud rate");
            return Err(BaudRateError);
        }
        info!("Serial baud rate changing to {}", hz);
        // The backend detail is logged here; the driver sees only the
        // opaque error and fails bring-up so the miner resets and
        // retries instead of mining on a misclocked line.
        if let Err(e) = self.serial.set_baudrate(hz) {
            error!("Serial baud switch failed: {:?}", Debug2Format(&e));
            return Err(BaudRateError);
        }
        Ok(())
    }

    async fn delay_ms(&mut self, ms: u32) {
        Timer::after(Duration::from_millis(u64::from(ms))).await;
    }
}

impl Serial {
    /// Construct a `Serial` for hashboard index `chain` of `chains` total
    /// chains the board wires up.
    #[must_use]
    pub const fn new(chain: usize, chains: usize) -> Self {
        Self {
            cmd: Channel::new(),
            reply: Signal::new(),
            caller: Mutex::new(0),
            chain,
            chains,
            pool_weights: Watch::new(),
            parked: AtomicBool::new(false),
        }
    }

    /// One actor per chain of a machine with `N` chains, as the `static`
    /// a board owns: `static CHAINS: [Serial; N] = Serial::table();`.
    ///
    /// The repeat expression builds `N` actors that all claim chain
    /// zero, and the loop then writes each its own index. Only the index
    /// is assigned, so nothing the repeat built is overwritten whole and
    /// const evaluation has no value to drop. `ubos_metrics::chain`'s
    /// `CHAIN_STATS` table is built the same way.
    #[must_use]
    pub const fn table<const N: usize>() -> [Self; N] {
        let mut slots = [const { Self::new(0, N) }; N];
        let mut chain = 0;
        while chain < N {
            slots[chain].chain = chain;
            chain += 1;
        }
        slots
    }

    /// Pair each actor of a board's [`Self::table`] with the reset pin
    /// in the same position, in the shape [`crate::Miner::new`] takes.
    ///
    /// A board states its chain order once, as the order of `resets`,
    /// instead of writing an index per chain into a literal array of
    /// pairs. That literal is where a wiring slip can put one chain's
    /// actor against another's reset pin, which `Miner::new` then
    /// catches only at boot; here there is no index to get wrong.
    ///
    /// `array::map` applies in order, so element `chain` of the result
    /// pairs actor `chain` with reset pin `chain`.
    #[must_use]
    pub fn wire<R, const N: usize>(
        actors: &'static [Self; N],
        resets: [R; N],
    ) -> [(&'static Self, R); N] {
        let mut chain = 0;
        resets.map(|reset| {
            let actor = &actors[chain];
            chain += 1;
            (actor, reset)
        })
    }

    /// This actor's index among the machine's hashboards.
    #[must_use]
    pub const fn chain(&self) -> usize {
        self.chain
    }

    /// The machine's hashboard count as this actor was built with it.
    /// [`crate::Miner::new`] checks it against the count the board
    /// declares, since an actor that partitions the version mask for a
    /// different number of chains would hand a sibling chain the same
    /// headers to hash.
    #[must_use]
    pub const fn chains(&self) -> usize {
        self.chains
    }

    /// Take the caller mutex, push a command stamped with a fresh
    /// sequence number, and await the reply tagged with that same number.
    /// The mutex guarantees the reply slot belongs to this caller; the
    /// tag guards against cancellation. The miner races serial commands
    /// against STOP and halt, so a caller can be dropped between `send`
    /// and its reply, leaving its command in flight; the server still
    /// answers it exactly once, and the next caller must discard that
    /// orphan rather than consume it as its own.
    async fn cmd_reply(&self, msg: SerialMessage) -> Reply {
        let mut next_seq = self.caller.lock().await;
        let seq = *next_seq;
        *next_seq = seq.wrapping_add(1);
        self.reply.reset();
        self.cmd.send((seq, msg)).await;
        loop {
            let (reply_seq, reply) = self.reply.wait().await;
            if reply_seq == seq {
                return reply;
            }
            // An orphaned reply to a cancelled predecessor: drop it and
            // keep waiting for this command's own answer.
        }
    }

    // The caller mutex serialises issuers and the server signals exactly
    // one reply per command, tagged with the command's sequence number;
    // cmd_reply discards tags it did not issue. A reply of the wrong
    // variant under a matching tag is therefore a protocol bug in this
    // file, not a runtime condition — the accessors below panic rather
    // than masking it as a result.

    fn unit(reply: Reply) -> Result<(), ErrorKind> {
        match reply {
            Reply::Unit(result) => result,
            _ => unreachable!("BUG: serial reply variant mismatch"),
        }
    }

    pub async fn terminate(&self) -> Result<(), ErrorKind> {
        Self::unit(self.cmd_reply(SerialMessage::Terminate).await)
    }

    /// Replace the scheduler's per-slot dispatch weights.
    ///
    /// Publishes to [`Self::pool_weights`] instead of sending a command,
    /// so it neither waits for a reply nor takes the caller mutex: the
    /// weights watcher must not sit behind a bring-up that runs for
    /// seconds. The actor applies them on its next pass through the loop,
    /// so a stopped chain picks them up when it next runs.
    pub fn set_pool_weights(&self, weights: [u32; crate::consts::MAX_POOL_SLOTS]) {
        self.pool_weights.sender().send(weights);
    }

    /// Re-arm the line at the chip's boot baud and clear pending work
    /// and driver state.
    pub async fn reset(&self) -> Result<(), ErrorKind> {
        Self::unit(self.cmd_reply(SerialMessage::Reset).await)
    }

    /// Reset and enumerate the chain, returning the chip count.
    pub async fn reset_and_enumerate(&self) -> u8 {
        match self.cmd_reply(SerialMessage::ResetEnumerate).await {
            Reply::Count(count) => count,
            _ => unreachable!("BUG: serial reply variant mismatch"),
        }
    }

    /// Configure the enumerated chain to its safe-boot operating point.
    pub async fn configure(&self) -> Result<(), ChipError> {
        match self.cmd_reply(SerialMessage::Configure).await {
            Reply::Freq(result) => result,
            _ => unreachable!("BUG: serial reply variant mismatch"),
        }
    }

    /// Read every chip's temperature and core voltage once. Reports
    /// `TimedOut` when no chip answered, which the miner treats as a
    /// failed bring-up step.
    pub async fn pvt_sweep(&self) -> Result<(), ErrorKind> {
        Self::unit(self.cmd_reply(SerialMessage::PvtSweep).await)
    }

    /// Program the hash-clock frequency.
    pub async fn set_frequency(&self, freq: Frequency) -> Result<(), ChipError> {
        match self.cmd_reply(SerialMessage::SetFrequency(freq)).await {
            Reply::Freq(result) => result,
            _ => unreachable!("BUG: serial reply variant mismatch"),
        }
    }

    /// Begin dispatching work at `period`.
    pub async fn start_sending_work(&self, period: Duration) -> Result<(), ErrorKind> {
        Self::unit(
            self.cmd_reply(SerialMessage::StartSendingWork(period))
                .await,
        )
    }

    /// Stop dispatching work.
    pub async fn stop_sending_work(&self) -> Result<(), ErrorKind> {
        Self::unit(self.cmd_reply(SerialMessage::StopSendingWork).await)
    }

    /// Retires the worker: masks the chip-UART RX interrupt for good, then
    /// decrements [`LIVE_ACTORS`].
    ///
    /// Once `run` returns nothing drains the UART, but the RX interrupt
    /// stays armed on the NVIC and a chip byte would still pend it —
    /// enough to wake the core out of the flash-pause `wfe` and fault it
    /// on suspended XIP. Masking removes that hazard for the rest of the
    /// image's life, which is what lets a flash write skip the park once
    /// [`core1_serial_running`] no longer counts this actor. The mask is
    /// published before the count so an observer cannot race an armed
    /// interrupt. If this actor was parked when it died, its
    /// [`PARKED_ACTORS`] count and `self.parked` flag are cleared too, so
    /// a park request cannot wait forever on an actor that died mid-park.
    fn stand_down(&self, serial: &mut impl BaudRate) {
        serial.set_rx_irq_enabled(false);
        if self.parked.swap(false, Ordering::Release) {
            PARKED_ACTORS.fetch_sub(1, Ordering::Release);
        }
        LIVE_ACTORS.fetch_sub(1, Ordering::Release);
    }

    /// If a park was requested, park this actor until it is released and
    /// return `true`.
    ///
    /// Called at the top of the actor loop, before the select. `select4`
    /// is biased by arm order, so when the UART RX buffer is continuously
    /// ready the read arm starves the park arm and the worker never parks
    /// within the requesting core's timeout. A chip that floods or browns
    /// out right after a rail cut drives exactly that continuously-ready
    /// condition. Checking here first guarantees the park is taken after
    /// at most one read is processed. The poll path keeps the same
    /// boundary: a bounded poll chunk finishes before parking. Cancelling
    /// a chunk could discard collected finds or interrupt a snapshot
    /// write. A park whose timeout is shorter than a chunk fails closed.
    /// Commands are the exception: a bring-up command holds the actor in
    /// `handle_command` for its full duration, and `ResetEnumerate` and
    /// `Configure` run for seconds across reset gaps, reply windows, and
    /// baud drains, so a park requested meanwhile is honoured only when
    /// the command finishes — an OTA colliding with a bring-up times out
    /// and fails closed rather than parking promptly. That fail-closed is
    /// deliberate, not a budget oversight: the OTA park-wait in
    /// `ubos-ota`'s `PARK_POLL_MAX_ATTEMPTS` is a synchronous core-0
    /// busy-wait, so — unlike a board reset task's budget, which awaits a
    /// timer and can be widened to sit through a bring-up — the OTA
    /// budget is left short on purpose: sizing it to a multi-second
    /// bring-up would stall embassy-net on core 0 for that whole window.
    /// The park loop yields to the embassy executor on every iteration
    /// via `embassy_futures::yield_now`, so every other task pending on
    /// the same core's executor gets a chance to run and reach its own
    /// park point in turn; a board with more than one chain on the same
    /// core relies on that to park all of them, and on [`PARK_WATCH`]
    /// holding a waker per actor so the request wakes each of them out
    /// of its select rather than only the one that registered last.
    /// Because the loop yields
    /// rather than busy-spinning, a test can also drive it by polling the
    /// returned future manually instead of needing on-hardware bring-up.
    async fn park_if_requested(&self, serial: &mut impl BaudRate) -> bool {
        if PARK_REQUESTED.load(Ordering::Acquire) == 0 {
            return false;
        }
        // Mask the RX interrupt before acking the park. The requester runs a
        // flash erase/program with XIP suspended; a pending chip-UART RX
        // interrupt would otherwise wake this core out of the flash driver's
        // `wfe` pause and fault it fetching the pause loop from suspended
        // flash. Restored on release.
        serial.set_rx_irq_enabled(false);
        self.parked.store(true, Ordering::Release);
        PARKED_ACTORS.fetch_add(1, Ordering::Release);
        while PARK_REQUESTED.load(Ordering::Acquire) > 0 {
            embassy_futures::yield_now().await;
        }
        PARKED_ACTORS.fetch_sub(1, Ordering::Release);
        self.parked.store(false, Ordering::Release);
        serial.set_rx_irq_enabled(true);
        true
    }

    /// Server entry point. Owns the byte stream and the work registry;
    /// holds a work receiver for every pool slot and asks `scheduler`
    /// which one to dispatch from on each work tick, then submits found
    /// shares back to the slot whose dispatch produced them through
    /// `session.submit_solution`. Reports every diff-1 chip share into
    /// `stats`, the chain's own [`ChainStats`], feeds the same shares to
    /// `scheduler.accumulate_share` so the delivered split tracks its
    /// configured policy, and records the chip count and desync bytes
    /// into `stats` too, so the split and the line health are observable
    /// through metrics per chain.
    ///
    /// Returns `Ok(())` when a [`SerialMessage::Terminate`] command was
    /// honoured, and `Err(BrokenPipe)` only on serial EOF. Line-level read
    /// errors are absorbed inside the `SerialIo` transport and the read
    /// arm so a transient FIFO overrun does not kill mining for the rest
    /// of the run.
    ///
    /// `board` and `driver` must describe the same chip: `board.chip` and
    /// `driver.descriptor()` are asserted identical below, so a caller
    /// wiring a board descriptor against the wrong driver panics here
    /// instead of tuning the wrong chip's frequency envelope.
    #[allow(clippy::too_many_lines)]
    pub async fn run<S, C, const N: usize, const CHAINS: usize, Sch: Scheduler<N>>(
        &self,
        mut serial: S,
        session: &'static AsicSession<N, CHAINS>,
        board: &'static HashboardDescriptor,
        mut driver: C,
        mut scheduler: Sch,
        stats: &'static ChainStats,
    ) -> Result<(), ErrorKind>
    where
        S: Read + Write + BaudRate,
        C: ChainDriver,
    {
        // Every real wire-up points `board.chip` and `driver.descriptor()`
        // at the same chip static: the descriptors are `static`, never
        // `const`, so a reference into one resolves to a single fixed
        // address program-wide no matter how many `HashboardDescriptor`s
        // copy it. A caller pairing a board against a driver for a
        // different chip is a wiring bug, not a runtime condition.
        assert!(
            core::ptr::eq(board.chip, driver.descriptor()),
            "BUG: board descriptor and chain driver disagree on the chip"
        );
        // One receiver per pool slot; `dispatch_work` asks the scheduler
        // which cell to read at each work tick, so a pool switch needs no
        // participation from the stratum sessions.
        let mut work_receivers: [DynReceiver<'_, WorkAssignment>; N] =
            core::array::from_fn(|slot| {
                session
                    .work_receiver(slot)
                    .expect("BUG: AsicSession Watch must have a free receiver slot")
            });
        let mut registry = WorkRegistry::new(driver.work_slots());
        let mut work_ticker: Option<Ticker> = None;
        let mut poll_cadence: Option<PollCadence> = None;
        let mut read_buf = [0u8; READ_CHUNK];
        // The version mask the last dispatched assignment granted, so a
        // grant this chain cannot split is reported once per grant
        // rather than once per work tick.
        let mut last_version_mask: Option<u32> = None;
        let mut park_wake = PARK_WATCH
            .receiver()
            .expect("BUG: park watch must have a receiver slot per chain");
        let mut pool_weights = self
            .pool_weights
            .receiver()
            .expect("BUG: a Serial's pool weights watch serves one actor");
        LIVE_ACTORS.fetch_add(1, Ordering::Release);

        loop {
            // Honor a park request before the select; see park_if_requested.
            if self.park_if_requested(&mut serial).await {
                continue;
            }

            // Weights the pool watcher published since the last pass.
            // Read here rather than in a command, so the watcher never
            // waits on this actor; see `Serial::set_pool_weights`.
            if let Some(weights) = pool_weights.try_changed() {
                scheduler.set_weights(&weights);
            }

            let result_mode = driver.result_mode();

            // Build the work-tick arm fresh each iteration. When work is
            // not enabled, this is a `Pending` future so the select only
            // waits on the other arms.
            let tick_arm = async {
                if let Some(t) = work_ticker.as_mut() {
                    t.next().await;
                } else {
                    core::future::pending::<()>().await;
                }
            };
            // Only readiness waits are cancellable. Once selected, a poll
            // chunk finishes and its results are processed before another
            // work tick, command or park can touch the driver.
            let result_arm = async {
                match result_mode {
                    ResultMode::Push => {
                        let outcome = serial.read(&mut read_buf).await.map_err(|e| e.kind());
                        ResultEvent::Read(outcome)
                    }
                    ResultMode::Poll { .. } => {
                        let Some(cadence) = poll_cadence.as_mut() else {
                            return core::future::pending::<ResultEvent>().await;
                        };
                        Timer::after(driver.poll_delay()).await;
                        ResultEvent::PollDue(cadence.next().await)
                    }
                }
            };
            let cmd_arm = self.cmd.receive();
            let park_arm = park_wake.changed();

            match select4(cmd_arm, park_arm, tick_arm, result_arm).await {
                Either4::Third(()) => {
                    self.dispatch_work(
                        &mut serial,
                        &mut scheduler,
                        &mut driver,
                        &mut registry,
                        &mut work_receivers,
                        &mut last_version_mask,
                    )
                    .await;
                }
                Either4::Fourth(ResultEvent::Read(Ok(0))) => {
                    error!("Serial port EOF");
                    stats.add_desync_bytes(driver.take_desync());
                    self.stand_down(&mut serial);
                    return Err(ErrorKind::BrokenPipe);
                }
                Either4::Fourth(ResultEvent::Read(Ok(n))) => {
                    let batch = driver.ingest(&read_buf[..n]);
                    Self::process_solutions(
                        &batch,
                        &registry,
                        session,
                        &mut scheduler,
                        &driver.difficulty_target(),
                        stats,
                    );
                    stats.add_desync_bytes(driver.take_desync());
                }
                Either4::Fourth(ResultEvent::Read(Err(kind))) => {
                    warn!("Serial read error: {:?} (recovering)", Debug2Format(&kind));
                    // A frame truncated by the fault must not splice with
                    // the bytes that follow it into a checksum-passing
                    // ghost; drop the driver's buffered partial frame.
                    driver.line_error();
                    stats.add_desync_bytes(driver.take_desync());
                }
                Either4::Fourth(ResultEvent::PollDue(status_turn)) => {
                    let mut io = SerialIo {
                        serial: &mut serial,
                    };
                    if status_turn {
                        driver.status_turn(&mut io).await;
                    }
                    let batch = driver.poll_results(&mut io).await;
                    Self::process_solutions(
                        &batch.solutions,
                        &registry,
                        session,
                        &mut scheduler,
                        &batch.difficulty,
                        stats,
                    );
                    if let Some(cadence) = poll_cadence.as_mut() {
                        cadence.progress = batch.progress;
                    }
                    stats.add_desync_bytes(driver.take_desync());
                }
                Either4::First((seq, msg)) => {
                    if let ControlFlow::Break(()) = self
                        .handle_command(
                            seq,
                            msg,
                            &mut serial,
                            &mut driver,
                            board,
                            &mut work_ticker,
                            &mut poll_cadence,
                            &mut registry,
                            stats,
                        )
                        .await
                    {
                        self.stand_down(&mut serial);
                        return Ok(());
                    }
                }
                Either4::Second(_requested) => {
                    // A park was requested while the worker was idle-blocked in
                    // the select. Just loop: the top-of-loop check performs the
                    // actual park, keeping the spin logic in one place.
                    // `serial.read` is cancel-safe, so dropping it here loses
                    // no data: the UART driver buffers incoming bytes in its
                    // ring buffer.
                }
            }
        }
    }

    /// Dispatch one work tick.
    ///
    /// `last_version_mask` carries the grant the previous dispatch saw,
    /// so a grant this chain cannot split is reported once per grant
    /// rather than once per tick. `run` owns it, since it lives for one
    /// run of the actor rather than for the life of the handle.
    async fn dispatch_work<S, const N: usize, Sch: Scheduler<N>>(
        &self,
        serial: &mut S,
        scheduler: &mut Sch,
        driver: &mut impl ChainDriver,
        registry: &mut WorkRegistry,
        work_receivers: &mut [DynReceiver<'_, WorkAssignment>; N],
        last_version_mask: &mut Option<u32>,
    ) where
        S: Read + Write + BaudRate,
    {
        // Ask the scheduler which slot to dispatch from this tick:
        // `evaluate` picks among the slots the availability mask marks as
        // holding work. `try_get` returns the slot's latest assignment
        // regardless of whether it changed since the last tick — the
        // chip is re-fed the current job every tick, picking up mid-job
        // target / merkle-root changes.
        let mut available: [bool; N] = core::array::from_fn(|i| work_receivers[i].contains_value());
        let (session_slot, assignment) = loop {
            let Some(slot) = scheduler.evaluate(&available) else {
                // Nothing dispatchable: every populated slot is either
                // excluded by the scheduler or empty. Skip the tick; the
                // select loop keeps servicing UART reads and commands.
                return;
            };
            if let Some(assignment) = work_receivers[slot].try_get() {
                break (slot, assignment);
            }
            // The session, which may run on another core, cleared its
            // cell between the mask build and the read. Retry without
            // this slot.
            available[slot] = false;
        };

        // Narrow the pool's granted mask to this chain's own share
        // before building the job, so sibling chains on the same
        // machine never hash identical headers.
        let job_version = JobVersion::partitioned(
            assignment.version,
            assignment.version_mask,
            self.chain,
            self.chains,
        );
        // A grant with fewer bits than the chain count needs leaves
        // every chain but the first with nothing to roll. Such a chain
        // would hash the headers chain 0 is already hashing, from the
        // same nonce start, and submit every share chain 0 submits, so
        // it is left idle instead: it can win the machine no hashrate
        // chain 0 is not already winning, and a pool counts what it
        // would send as duplicate shares.
        //
        // The version field is the only axis the chains are split on, so
        // there is nothing to fall back to here: the miner may vary only
        // the bits the pool granted, and moving a bit outside the grant
        // would have the pool rebuild a header the chain never hashed
        // and reject the share outright. Splitting `nonce_start` per
        // chain instead is what would put these chains back to work, and
        // is owed bench work; see `docs/ASIC.md`.
        //
        // The client asks for `min-bit-count: 2`, so a pool that
        // supports version rolling at all grants enough. This is the
        // pool that declines the extension, which leaves the negotiated
        // mask at zero. Reported at error level because the machine
        // loses the hashrate of every chain but the first, and once per
        // grant rather than once per work tick.
        let unsplittable = self.chain != 0 && self.chains > 1 && job_version.rolled_mask == 0;
        if *last_version_mask != Some(assignment.version_mask) {
            *last_version_mask = Some(assignment.version_mask);
            if unsplittable {
                error!(
                    "chain {}: the pool granted the version mask 0x{:08X}. It has too few bits for {} chains. This chain rolls no version bits, so it is left idle rather than hashing the same headers as chain 0 and submitting duplicate shares.",
                    self.chain, assignment.version_mask, self.chains
                );
            }
        }
        if unsplittable {
            return;
        }
        let work_id = registry.place(assignment, session_slot, job_version);
        let job = WorkJob {
            work_id,
            nonce_start: 0,
            nbits: assignment.nbits,
            ntime: assignment.ntime,
            version: job_version.version,
            version_mask: job_version.rolled_mask,
            merkle_root: assignment.merkle_root,
            prev_hash: assignment.prev_hash,
        };
        let mut io = SerialIo { serial };
        // No current driver returns an error from send_work — a raw write
        // failure surfaces one layer down in SerialIo — but the trait
        // allows a future family to refuse a job, so log and keep mining.
        if let Err(e) = driver.send_work(&mut io, &job).await {
            error!("Failed to send work: {:?}", Debug2Format(&e));
        }
    }

    /// Verify each solution against the chip's own difficulty target and the
    /// pool target, then submit the passing ones. Vendor-neutral: the
    /// header is rebuilt from the registry assignment and the
    /// driver-reported nonce, rolled version, and ntime offset, and the
    /// difficulty comes from the driver, since a chip family may filter
    /// harder than difficulty one to keep a slow link clear.
    fn process_solutions<const N: usize, const CHAINS: usize, Sch: Scheduler<N>>(
        batch: &[ChainSolution],
        registry: &WorkRegistry,
        session: &AsicSession<N, CHAINS>,
        scheduler: &mut Sch,
        difficulty: &[u8; 32],
        stats: &ChainStats,
    ) {
        for solution in batch {
            Self::process_solution(solution, registry, session, scheduler, difficulty, stats);
        }
    }

    fn process_solution<const N: usize, const CHAINS: usize, Sch: Scheduler<N>>(
        solution: &ChainSolution,
        registry: &WorkRegistry,
        session: &AsicSession<N, CHAINS>,
        scheduler: &mut Sch,
        difficulty: &[u8; 32],
        stats: &ChainStats,
    ) {
        let Some(slot) = registry.get(solution.work_id) else {
            warn!("No WorkAssignment for work_id={}", solution.work_id);
            return;
        };
        let assignment = &slot.assignment;

        // Merge the echo under the mask this chain was dispatched with,
        // which the registry kept from the dispatch. A driver may echo
        // only the rolled bits, or echo the whole field together with the
        // fixed bits. Either way the same header version is reconstructed.
        let version = slot.version.rolled(solution.version_bits);
        let ntime = assignment.ntime.wrapping_add(solution.ntime_offset);
        let hash = solution_hash(
            version,
            &assignment.prev_hash,
            &assignment.merkle_root,
            ntime,
            assignment.nbits,
            solution.nonce,
        );

        if !hash_meets_target(&hash, difficulty) {
            warn!(
                "nonce=0x{:08X} work_id={} below the chip's difficulty (hash={})",
                solution.nonce,
                solution.work_id,
                BlockHash(&hash)
            );
            return;
        }

        // Credit one share against the chip's difficulty target so the
        // chain's hashrate EWMA reflects the actual chip work rather than
        // only pool-passing shares. The estimator weights by target, so a
        // harder difficulty makes each collected solution count for more.
        stats.record_share(difficulty, slot.session_slot);
        scheduler.accumulate_share(slot.session_slot);

        if !hash_meets_target(&hash, &assignment.target) {
            trace!(
                "nonce=0x{:08X} work_id={} below pool target (hash={})",
                solution.nonce,
                solution.work_id,
                BlockHash(&hash)
            );
            return;
        }

        info!(
            "Submitting share: pool {} nonce=0x{:08X} work_id={} ver_bits=0x{:08X}",
            slot.session_slot, solution.nonce, solution.work_id, solution.version_bits
        );

        // The pool checks the submitted bits against its own grant, not
        // this chain's narrowed slice: that echo style carries only
        // the rolled index and is missing the fixed bits the pool still
        // expects to see, so submit the reconstructed header version
        // masked back down to the full grant instead of the driver's
        // raw echo.
        let submitted = Solution {
            assignment_id: assignment.assignment_id,
            nonce: solution.nonce,
            ntime,
            version_bits: version & assignment.version_mask,
        };
        if !session.submit_solution(slot.session_slot, submitted) {
            warn!("AsicSession solutions full, dropping submission");
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_command<S>(
        &self,
        seq: u32,
        msg: SerialMessage,
        serial: &mut S,
        driver: &mut impl ChainDriver,
        board: &'static HashboardDescriptor,
        work_ticker: &mut Option<Ticker>,
        poll_cadence: &mut Option<PollCadence>,
        registry: &mut WorkRegistry,
        stats: &ChainStats,
    ) -> ControlFlow<()>
    where
        S: Read + Write + BaudRate,
    {
        match msg {
            SerialMessage::Reset => {
                *work_ticker = None;
                *poll_cadence = None;
                registry.reset();
                // Drain the desync tally before the reset below zeroes
                // it with the rest of the driver state, then reset the
                // driver to clear its parser and mask cache and re-arm
                // the line at the chip's boot baud.
                stats.add_desync_bytes(driver.take_desync());
                driver.reset();
                stats.clear();
                let rate = driver.boot_uart_rate();
                let result = serial.set_baudrate(rate);
                match &result {
                    Ok(()) => info!("Serial reset to {} baud", rate),
                    Err(e) => error!("Serial reset baud switch failed: {:?}", Debug2Format(e)),
                }
                self.reply.signal((seq, Reply::Unit(result)));
            }
            SerialMessage::ResetEnumerate => {
                // This and Configure run the whole vendor bring-up inside
                // the command arm, so the core-1 park is unserviceable
                // until they return; see park_if_requested.
                let count = {
                    let mut io = SerialIo { serial };
                    driver.reset_and_enumerate(&mut io, board).await
                };
                stats.add_desync_bytes(driver.take_desync());
                // The count goes back to the miner, which stores it
                // with the presence it decides from it. Storing it here
                // as well would let the two disagree while the miner
                // retries a short chain.
                self.reply.signal((seq, Reply::Count(count)));
            }
            SerialMessage::Configure => {
                let result = {
                    let mut io = SerialIo { serial };
                    driver.configure(&mut io, board).await
                };
                let result = result
                    .inspect(|frequency| stats.set_frequency(*frequency))
                    .map(|_frequency| ());
                stats.add_desync_bytes(driver.take_desync());
                self.reply.signal((seq, Reply::Freq(result)));
            }
            SerialMessage::PvtSweep => {
                let answered = {
                    let mut io = SerialIo { serial };
                    driver.pvt_sweep(&mut io).await
                };
                stats.add_desync_bytes(driver.take_desync());
                // A sweep that read nothing is reported as a timeout
                // rather than as success. The miner spends a bring-up
                // attempt on it, so a chain only reaches `Present` once
                // a chip has actually answered.
                let result = if answered {
                    Ok(())
                } else {
                    Err(ErrorKind::TimedOut)
                };
                self.reply.signal((seq, Reply::Unit(result)));
            }
            SerialMessage::SetFrequency(freq) => {
                let result = {
                    let mut io = SerialIo { serial };
                    driver.set_frequency(&mut io, freq).await
                };
                stats.add_desync_bytes(driver.take_desync());
                if result.is_ok() {
                    stats.set_frequency(freq);
                }
                self.reply.signal((seq, Reply::Freq(result)));
            }
            SerialMessage::StartSendingWork(period) => {
                *work_ticker = Some(Ticker::every(period));
                if let ResultMode::Poll { period } = driver.result_mode() {
                    *poll_cadence = Some(PollCadence::every(period));
                }
                self.reply.signal((seq, Reply::Unit(Ok(()))));
            }
            SerialMessage::StopSendingWork => {
                *work_ticker = None;
                *poll_cadence = None;
                self.reply.signal((seq, Reply::Unit(Ok(()))));
            }
            SerialMessage::Terminate => {
                self.reply.signal((seq, Reply::Unit(Ok(()))));
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

/// Readiness on the poll path, or bytes read on the push path.
enum ResultEvent {
    Read(Result<usize, ErrorKind>),
    PollDue(bool),
}

/// Task shell that owns the UART and drives `actor` for the lifetime of
/// the firmware.
///
/// It never returns under normal operation: there is no SIGINT on bare
/// metal, so the actor only leaves its loop on a fatal serial error.
/// Rather than panic and reset the whole firmware, the shell then parks
/// so the rest of the system keeps running and a human can pull logs.
///
/// Each board owns a `static` array of `Serial`, one per hashboard
/// chain, built by [`Serial::table`], and passes it as `chains`. The
/// task pool takes its size from that array's length, so the two
/// cannot disagree, and [`crate::Miner::new`] checks each actor's own
/// index and chain count against the array before the machine mines.
///
/// `actor` is which entry of `chains` this instance of the task
/// drives, and the task asks
/// [`ubos_metrics::chain::ChainStatsHandle::chain`] for
/// `actor.chain()` to find that chain's accounting. The session and
/// driver types are parameters rather than names in this crate, because
/// the board composes both: the session's capacity is a per-board
/// const, and which chip family a board drives is its own business.
/// The board builds the driver where it spawns the task and hands it
/// in, so whatever the driver needs at construction, a chip constant
/// or a reference to a board-owned store, is wired at the spawn site
/// and the shell's signature is the same on every board.
#[macro_export]
macro_rules! serial_task {
    (chains = $chains:expr, $uart:ty, $session:ty, $board:expr, $driver:ty) => {
        #[embassy_executor::task(pool_size = $chains.len())]
        pub async fn serial_task(
            actor: &'static ubos_mining::serial::Serial,
            serial: $uart,
            session: &'static $session,
            driver: $driver,
        ) -> ! {
            /// Park interval after the actor's loop returns.
            const PARK_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(60);

            match actor
                .run(
                    serial,
                    session,
                    $board,
                    driver,
                    ubos_mining::scheduler::RatioScheduler::new(),
                    ubos_metrics::chain::handle().chain(actor.chain()),
                )
                .await
            {
                Ok(()) => defmt::info!("ubos-mining serial actor terminated cleanly"),
                Err(kind) => defmt::error!(
                    "ubos-mining serial actor exited: {:?}",
                    defmt::Debug2Format(&kind)
                ),
            }
            loop {
                embassy_time::Timer::after(PARK_INTERVAL).await;
            }
        }
    };
}
