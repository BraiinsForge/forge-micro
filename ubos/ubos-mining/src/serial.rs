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
//! cross-core signals in private statics, because every caller is on the
//! other core from the actor and holds no [`Serial`]: `ubos-ota` parks
//! core 1 before a flash write, and a board's `main` parks it at boot. A
//! signal in a static is the only thing that crosses from core 0 to core
//! 1, so a method on the handle could not serve them.

use core::fmt;
use core::ops::ControlFlow;

use embassy_futures::select::{Either4, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::DynReceiver;
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use embedded_io::ErrorKind;
use embedded_io_async::{Error as _, Read, Write};
use mining_protocol::hashing::hash_meets_target;
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use serial_baudrate::BaudRate;
use stratum_client::{AsicSession, Solution, WorkAssignment};
use ubos_asic::{
    BaudRateError, ChainDriver, ChainSolution, ChipError, HashboardDescriptor, JobVersion,
    PollProgress, ResultMode, Transport, WorkJob, solution_hash,
};
use ubos_core::units::Frequency;
use ubos_logging::{Debug2Format, error, info, trace, warn};

#[cfg(test)]
#[path = "tests/serial.rs"]
mod tests;

/// Lifetime-monotonic count of bytes discarded while scanning for a frame
/// preamble in the UART byte stream. Survives serial-actor recoveries;
/// useful for detecting sustained desync or noise. Drained from the
/// active [`ChainDriver`] after every interaction that can buffer bytes:
/// solution collections, line errors, and the command arms that run the
/// driver.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub(crate) static UART_DESYNC_BYTES: AtomicU32 = AtomicU32::new(0);

/// Lifetime-monotonic count of difficulty-1 shares per pool slot,
/// indexed by the slot the work was dispatched for. The per-slot view
/// of delivered work: the ratio between these counters is the ratio
/// the scheduler actually achieved.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub(crate) static POOL_WORK_SHARES: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

// ---------------------------------------------------------------------------
// Core-1 park primitives
//
// These three items coordinate a cooperative pause of the serial worker
// so a different core can run flash operations safely. The park loop uses
// a plain busy-spin so the code is portable across ARM (rp2350) and
// Xtensa (esp32s3) targets. `wfe` is only a power optimisation and is
// not needed for correctness; omitting it keeps `cortex_m` out of the
// ubos-mining dependency graph, satisfying the esp32s3 build constraint.
// The request functions are called from the OTA flash path and, on the
// boards that have one, from the firmware-reset path; a board that
// exercises neither leaves the park loop dormant for the image's
// lifetime.
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

/// Ack flag: `true` means the serial worker is currently spinning in the
/// park loop and will not attempt any flash or UART access.
///
/// Set to `true` immediately before the worker enters the busy-spin and
/// back to `false` after it exits. Callers that need exclusive flash
/// access should poll this with [`core1_is_parked`] after calling
/// [`request_core1_park`].
pub(crate) static PARKED: AtomicBool = AtomicBool::new(false);

/// `true` while [`Serial::run`] is on core 1 and able to answer a park
/// request.
///
/// A flash write that lands when this is `false` has nothing to park, and
/// waiting for an ack that can never come would fail the write. The case
/// that matters is a worker that has exited on serial EOF: its task then
/// idles forever, so every later config save would burn the writer's whole
/// park budget and fail. `run` masks the chip-UART RX interrupt on its way
/// out, so once this clears there is no interrupt left to wake core 1 out
/// of the flash-pause `wfe` either.
///
/// The flag is `false` for a short window at boot too, between core 1
/// arming the UART interrupt and reaching `run`. Nothing writes flash
/// there: config saves come from the API, which needs the network up.
static SERIAL_WORKER_LIVE: AtomicBool = AtomicBool::new(false);

/// Wakes the serial worker's `select` loop when a park is requested.
///
/// The worker may be blocked inside `serial.read` waiting for UART data when
/// a park request arrives. Signalling this wakes the `select4` so the
/// park arm fires promptly rather than waiting for the next UART byte.
/// Module-private: external callers park through [`request_core1_park`]
/// and friends, never by touching the signal directly.
static PARK_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Requests that the core-1 serial worker enter its park loop.
///
/// Increments [`PARK_REQUESTED`] with `Release` ordering and signals
/// [`PARK_SIGNAL`] to wake the worker's `select` loop immediately. After
/// this call, poll [`core1_is_parked`] to wait for the ack; flash
/// operations must not begin until the ack is observed. Pair every call
/// with exactly one [`release_core1_park`].
pub fn request_core1_park() {
    PARK_REQUESTED.fetch_add(1, Ordering::Release);
    PARK_SIGNAL.signal(());
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

/// Re-wakes the serial worker for a park that is already requested.
///
/// Signals [`PARK_SIGNAL`] without touching [`PARK_REQUESTED`], so a
/// requester polling for the ack can retry a cross-core wake that was lost
/// while core 1 sat in `wfe` with interrupts briefly disabled. Calling
/// [`request_core1_park`] again instead would leave an increment behind
/// that no release balances.
pub fn signal_core1_park() {
    PARK_SIGNAL.signal(());
}

/// Returns `true` while the core-1 serial worker is running and able to
/// answer a park request.
///
/// A flash writer must check this before waiting on [`core1_is_parked`]:
/// when it returns `false` there is no worker to park, and no chip-UART
/// interrupt armed on core 1 either, so the write is safe to run
/// unparked. See [`SERIAL_WORKER_LIVE`].
#[must_use]
pub fn core1_serial_running() -> bool {
    SERIAL_WORKER_LIVE.load(Ordering::Acquire)
}

/// Returns `true` while the core-1 serial worker is spinning in the park
/// loop.
///
/// Loads [`PARKED`] with `Acquire` ordering. Flash operations may proceed
/// once this returns `true`; the worker will not leave the park loop until
/// [`release_core1_park`] is called.
#[must_use]
pub fn core1_is_parked() -> bool {
    PARKED.load(Ordering::Acquire)
}

/// If a park was requested, spin until it is released and return `true`.
///
/// Called at the top of the actor loop, before the select. `select4` is
/// biased by arm order, so when the UART RX buffer is continuously ready
/// the read arm starves the park arm and the worker never parks within
/// the requesting core's timeout. A chip that floods or browns out right
/// after a rail cut drives exactly that continuously-ready condition.
/// Checking here first guarantees the park is
/// taken after at most one read is processed. The poll path keeps the
/// same boundary: a bounded poll chunk finishes before parking. Cancelling
/// a chunk could discard collected finds or interrupt a snapshot write.
/// A park whose timeout is shorter than a chunk fails closed. Commands are the exception: a bring-up command holds the actor
/// in `handle_command` for its full duration, and `ResetEnumerate` and
/// `Configure` run for seconds across reset gaps, reply windows, and
/// baud drains, so a park requested meanwhile is honoured only when the
/// command finishes — an OTA colliding with a bring-up times out and
/// fails closed rather than parking promptly. That fail-closed is
/// deliberate, not a budget oversight: the OTA park-wait in
/// `ubos-ota`'s `PARK_POLL_MAX_ATTEMPTS` is a synchronous core-0
/// busy-wait, so — unlike a board reset task's budget, which awaits a
/// timer and can be widened to sit through a bring-up — the OTA budget
/// is left short on purpose: sizing it to a multi-second bring-up would
/// stall embassy-net on core 0 for that whole window. The busy-spin uses
/// `core::hint::spin_loop()` only so it compiles for esp32s3 (Xtensa) and
/// keeps interrupts enabled, leaving the SIO FIFO ISR responsive on RP
/// boards. The spin never yields to the executor, so a cooperative
/// single-threaded test harness cannot drive both this loop and the task
/// that releases it; the park handshake is exercised by the on-hardware
/// core-split bring-up rather than the unit suite.
fn park_if_requested(serial: &mut impl BaudRate) -> bool {
    if PARK_REQUESTED.load(Ordering::Acquire) == 0 {
        return false;
    }
    // Mask the RX interrupt before acking the park. The requester runs a
    // flash erase/program with XIP suspended; a pending chip-UART RX
    // interrupt would otherwise wake this core out of the flash driver's
    // `wfe` pause and fault it fetching the pause loop from suspended
    // flash. Restored on release.
    serial.set_rx_irq_enabled(false);
    PARKED.store(true, Ordering::Release);
    while PARK_REQUESTED.load(Ordering::Acquire) > 0 {
        core::hint::spin_loop();
    }
    PARKED.store(false, Ordering::Release);
    serial.set_rx_irq_enabled(true);
    true
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

/// This actor's index among the machine's hashboards, and the total
/// hashboard count, used to partition a pool-granted version-rolling
/// mask so sibling chains never hash identical headers. Fixed at 0 of 1
/// for now; a later change gives each `Serial` its own index once a
/// board wires up more than one chain.
const CHAIN: usize = 0;
const CHAINS: usize = 1;

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
    /// first reading after enumeration.
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
    /// Replace the scheduler's per-slot dispatch weights.
    /// Fire-and-forget: no reply is signalled.
    SetPoolWeights([u32; 8]),
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
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cmd: Channel::new(),
            reply: Signal::new(),
            caller: Mutex::new(0),
        }
    }

    /// Take the caller mutex, push a command stamped with a fresh
    /// sequence number, and await the reply tagged with that same number.
    /// The mutex guarantees the reply slot belongs to this caller; the
    /// tag guards against cancellation. The miner races tuner init
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

    /// Read every chip's temperature and core voltage once.
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

    pub async fn start_sending_work(&self, period: Duration) -> Result<(), ErrorKind> {
        Self::unit(
            self.cmd_reply(SerialMessage::StartSendingWork(period))
                .await,
        )
    }

    pub async fn stop_sending_work(&self) -> Result<(), ErrorKind> {
        Self::unit(self.cmd_reply(SerialMessage::StopSendingWork).await)
    }

    pub async fn terminate(&self) -> Result<(), ErrorKind> {
        Self::unit(self.cmd_reply(SerialMessage::Terminate).await)
    }

    /// Replace the scheduler's per-slot dispatch weights without
    /// waiting for a reply. Applied directly in `run`'s command arm
    /// rather than through `handle_command`, so no reply is signalled
    /// for the sequence number this stamps. The send still takes the
    /// shared `caller` mutex, so it can wait behind an in-flight
    /// command's full round trip.
    pub async fn set_pool_weights(&self, weights: [u32; 8]) {
        let mut next_seq = self.caller.lock().await;
        let seq = *next_seq;
        *next_seq = seq.wrapping_add(1);
        self.cmd
            .send((seq, SerialMessage::SetPoolWeights(weights)))
            .await;
    }

    /// Retires the worker: masks the chip-UART RX interrupt for good, then
    /// clears [`SERIAL_WORKER_LIVE`].
    ///
    /// Once `run` returns nothing on core 1 drains the UART, but the RX
    /// interrupt stays armed on core 1's NVIC and a chip byte would still
    /// pend it — enough to wake core 1 out of the flash-pause `wfe` and
    /// fault it on suspended XIP. Masking removes that hazard for the rest
    /// of the image's life, which is what lets a flash write skip the park
    /// once [`core1_serial_running`] reads `false`. The mask is published
    /// before the flag so an observer of `false` cannot race an armed
    /// interrupt.
    fn stand_down(serial: &mut impl BaudRate) {
        serial.set_rx_irq_enabled(false);
        SERIAL_WORKER_LIVE.store(false, Ordering::Release);
    }

    /// Server entry point. Owns the byte stream and the work registry;
    /// holds a work receiver for every pool slot and asks `scheduler`
    /// which one to dispatch from on each work tick, then submits found
    /// shares back to the slot whose dispatch produced them through
    /// `session.submit_solution`. Bumps
    /// `ubos_metrics::hashrate::record_share` for every diff-1 chip
    /// share so the existing hashrate task picks them up, feeds the
    /// same shares to `scheduler.accumulate_share` so the delivered
    /// split tracks its configured policy, and bumps
    /// [`POOL_WORK_SHARES`] for the originating slot so the split is
    /// observable through metrics.
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
        SERIAL_WORKER_LIVE.store(true, Ordering::Release);

        loop {
            // Honor a park request before the select; see park_if_requested.
            if park_if_requested(&mut serial) {
                continue;
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
            let park_arm = PARK_SIGNAL.wait();

            match select4(cmd_arm, park_arm, tick_arm, result_arm).await {
                Either4::Third(()) => {
                    Self::dispatch_work(
                        &mut serial,
                        &mut scheduler,
                        &mut driver,
                        &mut registry,
                        &mut work_receivers,
                    )
                    .await;
                }
                Either4::Fourth(ResultEvent::Read(Ok(0))) => {
                    error!("Serial port EOF");
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                    Self::stand_down(&mut serial);
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
                    );
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                }
                Either4::Fourth(ResultEvent::Read(Err(kind))) => {
                    warn!("Serial read error: {:?} (recovering)", Debug2Format(&kind));
                    // A frame truncated by the fault must not splice with
                    // the bytes that follow it into a checksum-passing
                    // ghost; drop the driver's buffered partial frame.
                    driver.line_error();
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
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
                    );
                    if let Some(cadence) = poll_cadence.as_mut() {
                        cadence.progress = batch.progress;
                    }
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                }
                Either4::First((_, SerialMessage::SetPoolWeights(weights))) => {
                    scheduler.set_weights(&weights);
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
                        )
                        .await
                    {
                        Self::stand_down(&mut serial);
                        return Ok(());
                    }
                }
                Either4::Second(()) => {
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

    async fn dispatch_work<S, const N: usize, Sch: Scheduler<N>>(
        serial: &mut S,
        scheduler: &mut Sch,
        driver: &mut impl ChainDriver,
        registry: &mut WorkRegistry,
        work_receivers: &mut [DynReceiver<'_, WorkAssignment>; N],
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
        let job_version =
            JobVersion::partitioned(assignment.version, assignment.version_mask, CHAIN, CHAINS);
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
    ) {
        for solution in batch {
            Self::process_solution(solution, registry, session, scheduler, difficulty);
        }
    }

    fn process_solution<const N: usize, const CHAINS: usize, Sch: Scheduler<N>>(
        solution: &ChainSolution,
        registry: &WorkRegistry,
        session: &AsicSession<N, CHAINS>,
        scheduler: &mut Sch,
        difficulty: &[u8; 32],
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

        // Credit one share against the chip's difficulty target so the shared
        // hashrate EWMA reflects the actual chip work rather than only
        // pool-passing shares. The estimator weights by target, so a
        // harder difficulty makes each collected solution count for more.
        ubos_metrics::hashrate::record_share(difficulty);
        scheduler.accumulate_share(slot.session_slot);
        if let Some(counter) = POOL_WORK_SHARES.get(slot.session_slot) {
            counter.fetch_add(1, Ordering::Relaxed);
        }

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
                UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                driver.reset();
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
                UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                self.reply.signal((seq, Reply::Count(count)));
            }
            SerialMessage::Configure => {
                let result = {
                    let mut io = SerialIo { serial };
                    driver.configure(&mut io, board).await
                };
                UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                self.reply.signal((seq, Reply::Freq(result)));
            }
            SerialMessage::PvtSweep => {
                {
                    let mut io = SerialIo { serial };
                    driver.pvt_sweep(&mut io).await;
                }
                UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                self.reply.signal((seq, Reply::Unit(Ok(()))));
            }
            SerialMessage::SetFrequency(freq) => {
                let result = {
                    let mut io = SerialIo { serial };
                    driver.set_frequency(&mut io, freq).await
                };
                UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
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
            SerialMessage::SetPoolWeights(_) => {
                // Intercepted in `run`'s command arm before it reaches
                // `handle_command`; never actually matched here.
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

impl Default for Serial {
    fn default() -> Self {
        Self::new()
    }
}

/// Single workspace serial actor. One ASIC chain per process; matches
/// `ubos_mining::psu::PSU` in shape so consumers reach in directly rather than
/// wiring a static through every task constructor.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub static SERIAL: Serial = Serial::new();

/// Task shell that owns the UART and drives [`SERIAL`] for the lifetime
/// of the firmware.
///
/// It never returns under normal operation: there is no SIGINT on bare
/// metal, so the actor only leaves its loop on a fatal serial error.
/// Rather than panic and reset the whole firmware, the shell then parks
/// so the rest of the system keeps running and a human can pull logs.
///
/// The session and driver types are parameters rather than names in
/// this crate, because the board composes both: the session's capacity
/// is a per-board const, and which chip family a board drives is its
/// own business. The board builds the driver where it spawns the task
/// and hands it in, so whatever the driver needs at construction, a
/// chip constant or a reference to a board-owned store, is wired at the
/// spawn site and the shell's signature is the same on every board.
#[macro_export]
macro_rules! serial_task {
    ($uart:ty, $session:ty, $board:expr, $driver:ty) => {
        #[embassy_executor::task]
        pub async fn serial_task(serial: $uart, session: &'static $session, driver: $driver) -> ! {
            /// Park interval after the actor's loop returns.
            const PARK_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(60);

            match ubos_mining::serial::SERIAL
                .run(
                    serial,
                    session,
                    $board,
                    driver,
                    ubos_mining::scheduler::RatioScheduler::new(),
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
