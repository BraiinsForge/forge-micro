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

use core::fmt;
use core::ops::ControlFlow;

use embassy_futures::select::{Either4, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::DynReceiver;
use embassy_time::{Duration, Ticker, Timer, with_timeout};
use embedded_io::ErrorKind;
use embedded_io_async::{Error as _, Read, Write};
use mining_protocol::hashing::hash_meets_target;
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use serial_baudrate::BaudRate;
use stratum_client::{AsicSession, Solution, WorkAssignment};
use ubos_asic::{
    BaudRateError, ChainDriver, ChainSolution, ChipError, HashboardDescriptor, ResultMode,
    SolutionBatch, Transport, WorkJob, rolled_version, solution_hash,
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

/// Level flag: `true` means the serial worker should enter the park loop.
///
/// Set to `true` by [`request_core1_park`] and back to `false` by
/// [`release_core1_park`]. The serial worker polls this inside the park
/// loop and exits the spin when it returns `false`.
pub(crate) static PARK_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ack flag: `true` means the serial worker is currently spinning in the
/// park loop and will not attempt any flash or UART access.
///
/// Set to `true` immediately before the worker enters the busy-spin and
/// back to `false` after it exits. Callers that need exclusive flash
/// access should poll this with [`core1_is_parked`] after calling
/// [`request_core1_park`].
pub(crate) static PARKED: AtomicBool = AtomicBool::new(false);

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
/// Sets [`PARK_REQUESTED`] to `true` with `Release` ordering and signals
/// [`PARK_SIGNAL`] to wake the worker's `select` loop immediately. After
/// this call, poll [`core1_is_parked`] to wait for the ack; flash
/// operations must not begin until the ack is observed.
pub fn request_core1_park() {
    PARK_REQUESTED.store(true, Ordering::Release);
    PARK_SIGNAL.signal(());
}

/// Releases the core-1 serial worker from its park loop.
///
/// Sets [`PARK_REQUESTED`] to `false` with `Release` ordering. The worker
/// exits its busy-spin on the next poll of [`PARK_REQUESTED`] and resumes
/// normal operation. Safe to call when the worker is not parked (the store
/// is idempotent).
pub fn release_core1_park() {
    PARK_REQUESTED.store(false, Ordering::Release);
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
/// same bound: the nonce-FIFO poll runs inside the select, so the park
/// signal cancels an in-flight poll rather than waiting out its read
/// loop. Commands are the exception: a bring-up command holds the actor
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
fn park_if_requested() -> bool {
    if !PARK_REQUESTED.load(Ordering::Acquire) {
        return false;
    }
    PARKED.store(true, Ordering::Release);
    while PARK_REQUESTED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    PARKED.store(false, Ordering::Release);
    true
}

use crate::consts::WORK_REGISTRY_SLOTS;
use crate::scheduler::Scheduler;

/// How often the actor polls the nonce FIFO for poll-delivery chips
/// while work is active. Provisional, for a poll-family driver whose
/// cadence no hardware is available to calibrate here.
const POLL_PERIOD: Duration = Duration::from_millis(100);

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

/// In-flight work for a given `job_key` slot. Holds the assignment so an
/// incoming [`ChainSolution`] can be verified and mapped back to the
/// originating job. The assignment is captured at dispatch time because
/// the session's `Watch` publishes fresh assignments mid-job; shares are
/// credited against the assignment the chip was actually scanning.
struct WorkSlot {
    assignment: WorkAssignment,
    /// Pool slot whose work cell this dispatch was drawn from. The
    /// eventual solution routes back to this slot even if the
    /// scheduler picks other slots while the chip is hashing, so a
    /// session keeps submitting its in-flight shares.
    session_slot: usize,
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

    async fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> usize {
        match with_timeout(
            Duration::from_millis(u64::from(timeout_ms)),
            self.serial.read(buf),
        )
        .await
        {
            Ok(Ok(n)) => {
                trace!("RX ({} bytes): {}", n, HexBytes(&buf[..n]));
                n
            }
            Ok(Err(e)) => {
                warn!("Serial read error: {:?}", Debug2Format(&e));
                0
            }
            Err(_) => 0,
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
    /// Difficulty-1 target in 256-bit little-endian byte order: `0xFFFF * 2^208`.
    /// Matches Bitcoin's `bnPowLimit` for mainnet/regtest. Used to reject
    /// chip-emitted nonces that don't even meet the easiest possible target.
    const DIFF_1_TARGET: [u8; 32] = {
        let mut bytes = [0u8; 32];
        bytes[26] = 0xFF;
        bytes[27] = 0xFF;
        bytes
    };

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
    pub async fn run<S, C, const N: usize, Sch: Scheduler<N>>(
        &self,
        mut serial: S,
        session: &'static AsicSession<N>,
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
        // Registry index space; the driver contract guarantees at least
        // one job slot, so only the ceiling needs clamping here. The
        // driver maps job_key onto the chip's echo field within this
        // range, and the clamp keeps the cast lossless.
        let slots = driver.job_slots();
        assert!(slots >= 1, "BUG: driver reports zero job slots");
        if slots > WORK_REGISTRY_SLOTS {
            warn!(
                "Driver offers {} job slots, registry truncates to {}",
                slots, WORK_REGISTRY_SLOTS
            );
        }
        #[allow(clippy::cast_possible_truncation)]
        let job_slots = slots.min(WORK_REGISTRY_SLOTS) as u32;
        let mut work_ticker: Option<Ticker> = None;
        let mut poll_ticker: Option<Ticker> = None;
        let mut next_job_key: u32 = 0;
        let mut registry: [Option<WorkSlot>; WORK_REGISTRY_SLOTS] =
            [const { None }; WORK_REGISTRY_SLOTS];
        let mut read_buf = [0u8; READ_CHUNK];

        loop {
            // Honor a park request before the select; see park_if_requested.
            if park_if_requested() {
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
            // The result arm differs by delivery model: push chips are read
            // passively off the line; poll chips wait a ticker then read
            // their nonce FIFO. The whole poll runs inside the arm so a
            // command or park signal cancels a slow poll instead of
            // queueing behind its read loop: the driver's parser and the
            // UART ring buffer persist across a dropped poll, so no data
            // is lost and the next tick collects it. A request write
            // cancelled mid-frame costs one chip-side resync. `serial`
            // and `driver` are only borrowed for the duration of the
            // select. The `BrokenPipe` EOF exit below exists only on the
            // push arm: the poll arm reads through `Transport::read`,
            // whose contract folds end-of-stream into the same `0` as a
            // quiet line, so a poll-mode chain cannot observe stream
            // death and keeps collecting empty batches. A poll-family
            // driver that needs a dead link to restart mining must
            // detect the silence itself, for example via a
            // consecutive-empty-poll threshold surfaced through its own
            // error path. The push-arm `BrokenPipe` supervisor restart
            // does not cover it.
            let result_arm = async {
                match result_mode {
                    ResultMode::Push => {
                        let outcome = serial.read(&mut read_buf).await.map_err(|e| e.kind());
                        ResultEvent::Read(outcome)
                    }
                    ResultMode::Poll => {
                        if let Some(t) = poll_ticker.as_mut() {
                            t.next().await;
                            let mut io = SerialIo {
                                serial: &mut serial,
                            };
                            ResultEvent::Polled(driver.poll_results(&mut io).await)
                        } else {
                            core::future::pending::<ResultEvent>().await
                        }
                    }
                }
            };
            let cmd_arm = self.cmd.receive();
            let park_arm = PARK_SIGNAL.wait();

            match select4(tick_arm, result_arm, cmd_arm, park_arm).await {
                Either4::First(()) => {
                    Self::dispatch_work(
                        &mut serial,
                        &mut scheduler,
                        &mut driver,
                        &mut next_job_key,
                        job_slots,
                        &mut registry,
                        &mut work_receivers,
                    )
                    .await;
                }
                Either4::Second(ResultEvent::Read(Ok(0))) => {
                    error!("Serial port EOF");
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                    return Err(ErrorKind::BrokenPipe);
                }
                Either4::Second(ResultEvent::Read(Ok(n))) => {
                    let batch = driver.ingest(&read_buf[..n]);
                    Self::process_solutions(&batch, &registry, session, &mut scheduler);
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                }
                Either4::Second(ResultEvent::Read(Err(kind))) => {
                    warn!("Serial read error: {:?} (recovering)", Debug2Format(&kind));
                    // A frame truncated by the fault must not splice with
                    // the bytes that follow it into a checksum-passing
                    // ghost; drop the driver's buffered partial frame.
                    driver.line_error();
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                }
                Either4::Second(ResultEvent::Polled(batch)) => {
                    Self::process_solutions(&batch, &registry, session, &mut scheduler);
                    UART_DESYNC_BYTES.fetch_add(driver.take_desync(), Ordering::Relaxed);
                }
                Either4::Third((_, SerialMessage::SetPoolWeights(weights))) => {
                    scheduler.set_weights(&weights);
                }
                Either4::Third((seq, msg)) => {
                    if let ControlFlow::Break(()) = self
                        .handle_command(
                            seq,
                            msg,
                            &mut serial,
                            &mut driver,
                            board,
                            &mut work_ticker,
                            &mut poll_ticker,
                            &mut next_job_key,
                            &mut registry,
                        )
                        .await
                    {
                        return Ok(());
                    }
                }
                Either4::Fourth(()) => {
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

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_work<S, const N: usize, Sch: Scheduler<N>>(
        serial: &mut S,
        scheduler: &mut Sch,
        driver: &mut impl ChainDriver,
        next_job_key: &mut u32,
        job_slots: u32,
        registry: &mut [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
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

        let job_key = *next_job_key;
        let job = WorkJob {
            job_key,
            nonce_start: 0,
            nbits: assignment.nbits,
            ntime: assignment.ntime,
            version: assignment.version,
            merkle_root: assignment.merkle_root,
            prev_hash: assignment.prev_hash,
        };
        // The registry slot mirrors `job_key`; the driver maps `job_key`
        // to and from the chip's echo field, so a returned solution finds
        // this same slot.
        registry[job_key as usize] = Some(WorkSlot {
            assignment,
            session_slot,
        });
        // `job_slots` is bounded by WORK_REGISTRY_SLOTS and is at least 1,
        // so this stays a valid registry index.
        *next_job_key = (job_key + 1) % job_slots;

        let mut io = SerialIo { serial };
        // No current driver returns an error from send_work — a raw write
        // failure surfaces one layer down in SerialIo — but the trait
        // allows a future family to refuse a job, so log and keep mining.
        if let Err(e) = driver.send_work(&mut io, &job).await {
            error!("Failed to send work: {:?}", Debug2Format(&e));
        }
    }

    /// Verify each solution against the diff-1 floor and the pool target,
    /// then submit the passing ones. Vendor-neutral: the header is rebuilt
    /// from the registry assignment and the driver-reported nonce, rolled
    /// version, and ntime offset.
    fn process_solutions<const N: usize, Sch: Scheduler<N>>(
        batch: &[ChainSolution],
        registry: &[Option<WorkSlot>; WORK_REGISTRY_SLOTS],
        session: &AsicSession<N>,
        scheduler: &mut Sch,
    ) {
        for solution in batch {
            Self::process_solution(solution, registry, session, scheduler);
        }
    }

    fn process_solution<const N: usize, Sch: Scheduler<N>>(
        solution: &ChainSolution,
        registry: &[Option<WorkSlot>; WORK_REGISTRY_SLOTS],
        session: &AsicSession<N>,
        scheduler: &mut Sch,
    ) {
        // `job_key` derives from the chip-returned echo field, so a stray
        // reply can land past the registry. `get` keeps that off the wire
        // from panicking; it folds into the missing-assignment path.
        let Some(slot) = registry
            .get(solution.job_key as usize)
            .and_then(Option::as_ref)
        else {
            warn!("No WorkAssignment for job_key={}", solution.job_key);
            return;
        };
        let assignment = &slot.assignment;

        let version = rolled_version(assignment.version, solution.version_bits);
        let ntime = assignment.ntime.wrapping_add(solution.ntime_offset);
        let hash = solution_hash(
            version,
            &assignment.prev_hash,
            &assignment.merkle_root,
            ntime,
            assignment.nbits,
            solution.nonce,
        );

        if !hash_meets_target(&hash, &Self::DIFF_1_TARGET) {
            warn!(
                "nonce=0x{:08X} job_key={} below diff 1 (hash={})",
                solution.nonce,
                solution.job_key,
                BlockHash(&hash)
            );
            return;
        }

        // Credit one share against the diff-1 target so the shared
        // hashrate EWMA reflects the actual chip work rather than only
        // pool-passing shares. The estimator weights by target.
        ubos_metrics::hashrate::record_share(&Self::DIFF_1_TARGET);
        scheduler.accumulate_share(slot.session_slot);
        if let Some(counter) = POOL_WORK_SHARES.get(slot.session_slot) {
            counter.fetch_add(1, Ordering::Relaxed);
        }

        if !hash_meets_target(&hash, &assignment.target) {
            trace!(
                "nonce=0x{:08X} job_key={} below pool target (hash={})",
                solution.nonce,
                solution.job_key,
                BlockHash(&hash)
            );
            return;
        }

        info!(
            "Submitting share: pool {} nonce=0x{:08X} job_key={} ver_bits=0x{:08X}",
            slot.session_slot, solution.nonce, solution.job_key, solution.version_bits
        );

        let submitted = Solution {
            work_id: assignment.work_id,
            nonce: solution.nonce,
            ntime,
            version_bits: solution.version_bits,
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
        poll_ticker: &mut Option<Ticker>,
        next_job_key: &mut u32,
        registry: &mut [Option<WorkSlot>; WORK_REGISTRY_SLOTS],
    ) -> ControlFlow<()>
    where
        S: Read + Write + BaudRate,
    {
        match msg {
            SerialMessage::Reset => {
                *work_ticker = None;
                *poll_ticker = None;
                *next_job_key = 0;
                for slot in registry.iter_mut() {
                    *slot = None;
                }
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
                if driver.result_mode() == ResultMode::Poll {
                    *poll_ticker = Some(Ticker::every(POLL_PERIOD));
                }
                self.reply.signal((seq, Reply::Unit(Ok(()))));
            }
            SerialMessage::StopSendingWork => {
                *work_ticker = None;
                *poll_ticker = None;
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

/// What the result arm produced: bytes read on the push path, or the
/// solution batch a nonce-FIFO poll collected.
// No heap on this target, so the batch cannot be boxed; it is a
// transient stack value on the same path that always held one.
#[allow(clippy::large_enum_variant)]
enum ResultEvent {
    Read(Result<usize, ErrorKind>),
    Polled(SolutionBatch),
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
