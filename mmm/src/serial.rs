//! Single-task actor for the BM1362 chip serial line.
//!
//! Layout mirrors the redacted `mmm`: one task owns the serial port
//! and a registry of in-flight work; outside callers issue commands
//! (reset, baud-rate change, start/stop work dispatch) and dump pre-built
//! command frames through a queue. The actor `select`s among its work
//! timer, incoming bytes from the chip, and the command channel.
//!
//! Key differences from the std parent:
//! - `embassy_sync::Channel` replaces `tokio::sync::mpsc`; per-call replies
//!   come back through a `Signal` slot guarded by a caller mutex (rather
//!   than per-call `oneshot`s) so the cmd payloads stay `'static`-clean
//!   and embedabble in a `Channel`.
//! - work source / share sink wire directly to `ubos_backend::next_work`
//!   and `ubos_backend::SHARES`; no separate stratum handle.
//! - hashrate is reported by bumping `ubos_backend::TOTAL_HASHES` per
//!   diff-1 chip share (`2^32` hashes each); the existing
//!   `ubos_backend::hashrate_task` does the rate calculation.

use core::fmt;

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Ticker, Timer};
use embedded_io::ErrorKind;
use embedded_io_async::{Error as _, Read, Write};
use portable_atomic::Ordering;
use serial_baudrate::BaudRate;
use ubos_backend::SHARES;
use ubos_backend::Share;
use ubos_backend::TOTAL_HASHES;
use ubos_backend::bitcoin::Target;
use ubos_logging::{Debug2Format, error, info, trace, warn};

use crate::bm1362::{Frame, HwWork, Response, ResponseParser};
use crate::consts::{
    DIFFICULTY, FAST_BAUDRATE, FAST_BAUDRATE_ACTUAL, INITIAL_BAUDRATE, MAX_CHIPS, NUM_MIDSTATES,
    REGISTRY_SIZE,
};

const CMD_QUEUE_DEPTH: usize = 16;

/// One diff-1 hardware share at 32-bit nonce search ≈ 2^32 hashes.
const HASHES_PER_SHARE: u64 = 1u64 << 32;

/// Drain time around a baud-rate change. Original redacted sleeps on
/// either side of the configure call to let the chip's UART catch up.
const BAUD_CHANGE_DRAIN: Duration = Duration::from_secs(1);

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

/// Outbound command from a [`Serial`] handle to the server task. Replies
/// for the variants that need one come back through `Serial::reply`.
#[derive(Debug, Clone)]
pub enum SerialMessage {
    /// Re-arm the line at [`INITIAL_BAUDRATE`] and clear pending work.
    Reset,
    /// Re-tune the line. Sends the request through the standard drain
    /// dance the chip needs around a rate change.
    SetBaudRate(u32),
    /// Begin dispatching work at `period`. The first tick fires immediately.
    StartSendingWork(Duration),
    /// Stop dispatching work. Any in-flight chip responses still arrive.
    StopSendingWork,
    /// Tell the server task to exit its loop.
    Terminate,
    /// Fire-and-forget: enqueue a pre-built command frame to be written
    /// to the chip. No reply is signalled.
    SendFrame(Frame),
}

/// In-flight work for a given `work_id` slot. Kept around so an incoming
/// `HwSolution` can be matched back to the originating job when building
/// the [`Share`] to push into [`SHARES`].
#[derive(Clone)]
struct WorkSlot {
    hw_work: HwWork,
    job: ubos_backend::Job,
}

/// The chip-side serial actor.
///
/// Construct as a `static`: every primitive inside is `const`-initialisable.
/// Callers send commands through the typed methods; the server task is
/// driven by [`Self::run`] on the same instance.
pub struct Serial {
    cmd: Channel<CriticalSectionRawMutex, SerialMessage, CMD_QUEUE_DEPTH>,
    /// Reply slot for the variant of the most recently issued command
    /// that needs one. The caller mutex serialises issuers so two callers
    /// cannot race on this single signal.
    reply: Signal<CriticalSectionRawMutex, Result<(), ErrorKind>>,
    /// Overflow queue for register-read responses so a follow-on read
    /// after a write can drain prior replies before issuing its own.
    /// Single-consumer in practice; capacity matches the broadcast read
    /// fan-in (one reply per chip).
    register_replies: Channel<CriticalSectionRawMutex, Response, MAX_CHIPS>,
    /// Serialises caller-side `cmd.send + reply.wait` pairs so their
    /// reply slots never interleave. `NoopRawMutex` is correct because
    /// all callers share the executor.
    caller: Mutex<NoopRawMutex, ()>,
}

impl Serial {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cmd: Channel::new(),
            reply: Signal::new(),
            register_replies: Channel::new(),
            caller: Mutex::new(()),
        }
    }

    /// Take the caller mutex, push a command, and await its reply. The
    /// mutex guarantees the reply slot belongs to this caller; the
    /// server task always calls `reply.signal` exactly once per reply-
    /// bearing message, in receive order.
    async fn cmd_reply(&self, msg: SerialMessage) -> Result<(), ErrorKind> {
        let _g = self.caller.lock().await;
        self.reply.reset();
        self.cmd.send(msg).await;
        self.reply.wait().await
    }

    pub async fn reset(&self) -> Result<(), ErrorKind> {
        self.cmd_reply(SerialMessage::Reset).await
    }

    pub async fn set_baud_rate(&self, rate: u32) -> Result<(), ErrorKind> {
        self.cmd_reply(SerialMessage::SetBaudRate(rate)).await
    }

    pub async fn start_sending_work(&self, period: Duration) -> Result<(), ErrorKind> {
        self.cmd_reply(SerialMessage::StartSendingWork(period))
            .await
    }

    pub async fn stop_sending_work(&self) -> Result<(), ErrorKind> {
        self.cmd_reply(SerialMessage::StopSendingWork).await
    }

    pub async fn terminate(&self) -> Result<(), ErrorKind> {
        self.cmd_reply(SerialMessage::Terminate).await
    }

    /// Fire-and-forget command-frame submission. The frame is written to
    /// the chip in the order it arrives at the server task.
    pub async fn send_frame(&self, frame: Frame) {
        self.cmd.send(SerialMessage::SendFrame(frame)).await;
    }

    /// Pop the next register-read reply, if any has landed. Returns
    /// immediately; callers needing to wait should drive a `select` with
    /// a timeout themselves.
    pub fn try_register_response(&self) -> Option<Response> {
        self.register_replies.try_receive().ok()
    }

    /// Server entry point. Owns the byte stream and the work registry;
    /// produces shares into [`SHARES`] and bumps [`TOTAL_HASHES`] for
    /// every diff-1 chip share so the existing hashrate task picks them
    /// up.
    pub async fn run<R, T>(&self, mut rx: R, mut tx: T) -> !
    where
        R: Read + BaudRate,
        T: Write + BaudRate,
    {
        let mut parser = ResponseParser::new();
        let mut work_ticker: Option<Ticker> = None;
        let mut work_id: u8 = 0;
        let mut registry: [Option<WorkSlot>; REGISTRY_SIZE] = [const { None }; REGISTRY_SIZE];

        loop {
            // Build the work-tick arm fresh each iteration. When work is
            // not enabled, this is a `Pending` future so the select only
            // waits on the other two arms.
            let tick_arm = async {
                if let Some(t) = work_ticker.as_mut() {
                    t.next().await;
                } else {
                    core::future::pending::<()>().await;
                }
            };
            let read_arm = rx.read(parser.free());
            let cmd_arm = self.cmd.receive();

            match select3(tick_arm, read_arm, cmd_arm).await {
                Either3::First(()) => {
                    self.dispatch_work(&mut tx, &mut work_id, &mut registry)
                        .await;
                }
                Either3::Second(read_result) => {
                    if !Self::handle_read(
                        read_result,
                        &mut parser,
                        &registry,
                        &self.register_replies,
                    ) {
                        // EOF or read error — bail. The task macro requires `!`
                        // so we just park forever; the user can reset by
                        // restarting the executor.
                        loop {
                            Timer::after(Duration::from_secs(60)).await;
                        }
                    }
                }
                Either3::Third(msg) => {
                    self.handle_command(
                        msg,
                        &mut tx,
                        &mut parser,
                        &mut work_ticker,
                        &mut work_id,
                        &mut registry,
                    )
                    .await;
                }
            }
        }
    }

    async fn dispatch_work<T>(
        &self,
        tx: &mut T,
        work_id: &mut u8,
        registry: &mut [Option<WorkSlot>; REGISTRY_SIZE],
    ) where
        T: Write + BaudRate,
    {
        let job = ubos_backend::next_work().await;

        // Job stores time as BE bytes (matches mining.notify hex order);
        // HwWork wants u32s that, when emitted via to_le_bytes(), produce
        // LE wire bytes for the chip.
        let hw_work = HwWork {
            work_id: *work_id,
            nonce_start: 0,
            nbits: u32::from_be_bytes(job.bits),
            ntime: u32::from_be_bytes(job.time),
            merkle_root: job.merkle_root,
            prev_hash: job.prev_hash,
            version: u32::from_be_bytes(job.version),
        };
        let frame = hw_work.serialize();
        trace!(
            "TX work ({} bytes): {}",
            frame.len(),
            HexBytes(frame.as_slice())
        );

        let slot_idx = usize::from(*work_id) / NUM_MIDSTATES;
        registry[slot_idx] = Some(WorkSlot {
            hw_work,
            job: job.clone(),
        });
        // `NUM_MIDSTATES` is 8; the cast is exact and fits well within u8.
        #[allow(clippy::cast_possible_truncation)]
        let stride = NUM_MIDSTATES as u8;
        *work_id = work_id.wrapping_add(stride);

        if let Err(e) = tx.write_all(&frame).await {
            error!("Failed to send work: {:?}", Debug2Format(&e.kind()));
        }
    }

    /// Returns false if the read failed in a way that should stop the
    /// server (EOF, IO error). Logs the error before returning.
    fn handle_read<E: embedded_io::Error>(
        result: Result<usize, E>,
        parser: &mut ResponseParser,
        registry: &[Option<WorkSlot>; REGISTRY_SIZE],
        register_replies: &Channel<CriticalSectionRawMutex, Response, MAX_CHIPS>,
    ) -> bool {
        match result {
            Ok(0) => {
                error!("Serial port EOF");
                false
            }
            Ok(n) => {
                trace!("RX ({} bytes): {}", n, HexBytes(&parser.free()[..n]));
                parser.commit(n);
                while let Some(resp) = parser.pop() {
                    Self::dispatch_response(&resp, registry, register_replies);
                }
                true
            }
            Err(e) => {
                error!("Serial read error: {:?}", Debug2Format(&e.kind()));
                false
            }
        }
    }

    fn dispatch_response(
        resp: &Response,
        registry: &[Option<WorkSlot>; REGISTRY_SIZE],
        register_replies: &Channel<CriticalSectionRawMutex, Response, MAX_CHIPS>,
    ) {
        match resp {
            Response::HwSolution(wr) => {
                let registry_id = usize::from(wr.work_id) / NUM_MIDSTATES;
                let version_bits = (u32::from(wr.version_idx) << 13) & 0x1FFF_E000;

                let Some(slot) = registry[registry_id].as_ref() else {
                    warn!(
                        "No WorkAssignment for work_id={} (registry_id {})",
                        wr.work_id, registry_id
                    );
                    return;
                };
                let hash = slot.hw_work.compute_hash(wr);

                if !Target::DIFF_1.meets(&hash) {
                    warn!(
                        "nonce=0x{:08X} work_id={} ver_idx={} below diff 1 (hash={})",
                        wr.nonce, wr.work_id, wr.version_idx, hash
                    );
                    return;
                }

                // Each diff-1 hardware share is ~2^32 hashes; bump the
                // global counter so ubos_backend::hashrate_task picks
                // up the rate. DIFFICULTY is the per-share weight kept
                // around so a future config change to a diff>1 line is
                // a one-line edit.
                TOTAL_HASHES.fetch_add(HASHES_PER_SHARE * u64::from(DIFFICULTY), Ordering::Relaxed);

                if !slot.job.pool_target.meets(&hash) {
                    trace!(
                        "nonce=0x{:08X} work_id={} ver_idx={} below pool diff {} (hash={})",
                        wr.nonce, wr.work_id, wr.version_idx, slot.job.pool_difficulty, hash
                    );
                    return;
                }

                info!(
                    "Submitting share: nonce=0x{:08X} work_id={} ver_idx={} hash={} pool_diff={}",
                    wr.nonce, wr.work_id, wr.version_idx, hash, slot.job.pool_difficulty
                );

                let share = Share {
                    job_id: slot.job.job_id.clone(),
                    extranonce2: [0u8; 16],
                    extranonce2_size: slot.job.extranonce2_size,
                    time: slot.job.time,
                    nonce: wr.nonce,
                    version_bits,
                };
                if SHARES.try_send(share).is_err() {
                    warn!("SHARES full, dropping submission");
                }
            }
            Response::RegisterResponse(r) => {
                info!(
                    "RX register response: reg=0x{:02X} value=0x{:08X} chip=0x{:02X}",
                    r.reg, r.value, r.chip_addr
                );
                if register_replies.try_send(resp.clone()).is_err() {
                    warn!("register_replies full, dropping reply");
                }
            }
        }
    }

    async fn handle_command<T>(
        &self,
        msg: SerialMessage,
        tx: &mut T,
        parser: &mut ResponseParser,
        work_ticker: &mut Option<Ticker>,
        work_id: &mut u8,
        registry: &mut [Option<WorkSlot>; REGISTRY_SIZE],
    ) where
        T: Write + BaudRate,
    {
        match msg {
            SerialMessage::Reset => {
                parser.clear();
                *work_ticker = None;
                *work_id = 0;
                for slot in registry.iter_mut() {
                    *slot = None;
                }
                let result = tx.set_baudrate(INITIAL_BAUDRATE);
                info!("Serial reset to {} baud", INITIAL_BAUDRATE);
                self.reply.signal(result);
            }
            SerialMessage::SetBaudRate(rate) => {
                parser.clear();
                let actual = if rate == FAST_BAUDRATE {
                    FAST_BAUDRATE_ACTUAL
                } else {
                    rate
                };
                info!("Serial baud rate changing to {}, draining 1s...", rate);
                Timer::after(BAUD_CHANGE_DRAIN).await;
                let result = tx.set_baudrate(actual);
                info!("Serial baud rate changed to {}, settling 1s...", rate);
                Timer::after(BAUD_CHANGE_DRAIN).await;
                self.reply.signal(result);
            }
            SerialMessage::StartSendingWork(period) => {
                *work_ticker = Some(Ticker::every(period));
                self.reply.signal(Ok(()));
            }
            SerialMessage::StopSendingWork => {
                *work_ticker = None;
                self.reply.signal(Ok(()));
            }
            SerialMessage::SendFrame(frame) => {
                let data = frame.as_slice();
                trace!("TX ({} bytes): {}", data.len(), HexBytes(data));
                if let Err(e) = tx.write_all(data).await {
                    error!("Serial write error: {:?}", Debug2Format(&e.kind()));
                }
            }
            SerialMessage::Terminate => {
                self.reply.signal(Ok(()));
                // Park forever; the actor task contract is `-> !`. The
                // caller relies on terminate as a "drain and stop" signal
                // rather than an actual task drop, which embassy doesn't
                // support without a JoinHandle wrapper.
                loop {
                    Timer::after(Duration::from_secs(60)).await;
                }
            }
        }
    }
}

impl Default for Serial {
    fn default() -> Self {
        Self::new()
    }
}
