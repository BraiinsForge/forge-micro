//! Single-task actor for the BM1362 chip serial line.
//!
//! One task owns the serial halves and the in-flight work registry;
//! outside callers go through [`Serial`]'s typed methods to reset the
//! chip, retune the baud rate, gate work dispatch, or push a pre-built
//! command frame. The actor reads work from a `&'static AsicSession`'s
//! `Watch` and submits found shares back via `AsicSession::submit_solution`.

use core::fmt;
use core::ops::ControlFlow;

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::DynReceiver;
use embassy_time::{Duration, Ticker, Timer, with_timeout};
use embedded_io::ErrorKind;
use embedded_io_async::{Error as _, Read, Write};
use heapless::Vec as HVec;
use mining_protocol::hashing::hash_meets_target;
use portable_atomic::{AtomicU32, Ordering};
use serial_baudrate::BaudRate;
use stratum_client::{AsicSession, Solution, WorkAssignment};
use ubos_asic::{
    Frame, HwWork, RawChipAddress, Register, RegisterResponse, Response, ResponseParser,
    build_read_register, build_write_register,
};
use ubos_logging::{Debug2Format, error, info, trace, warn};

/// Lifetime-monotonic count of bytes discarded while scanning for the
/// `0xAA 0x55` frame preamble in the UART byte stream. Survives serial-
/// actor recoveries; useful for detecting sustained desync or noise.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub static UART_DESYNC_BYTES: AtomicU32 = AtomicU32::new(0);

use crate::consts::{
    FAST_BAUDRATE, FAST_BAUDRATE_ACTUAL, INITIAL_BAUDRATE, MAX_CHIPS, NUM_CHIPS, NUM_MIDSTATES,
    REGISTRY_SIZE,
};

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
/// `HwSolution` can be matched back to the originating assignment when
/// building the [`Solution`] to push through `AsicSession::submit_solution`.
/// The target is captured at dispatch time because the session's
/// `Watch` publishes fresh assignments mid-job; we credit shares
/// against the target the chip was actually scanning to.
#[derive(Clone)]
struct WorkSlot {
    hw_work: HwWork,
    assignment: WorkAssignment,
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
    cmd: Channel<CriticalSectionRawMutex, SerialMessage, CMD_QUEUE_DEPTH>,
    /// Reply slot for the variant of the most recently issued command
    /// that needs one. The caller mutex serialises issuers so two callers
    /// cannot race on this single signal.
    reply: Signal<CriticalSectionRawMutex, Result<(), ErrorKind>>,
    /// Queue of register-read responses produced by the parser. Owned by
    /// `read_register_raw` for the duration of a session; the session mutex
    /// keeps concurrent callers from stealing each other's replies.
    /// Capacity matches the broadcast read fan-in (one reply per chip).
    register_replies: Channel<CriticalSectionRawMutex, RegisterResponse, MAX_CHIPS>,
    /// Serialises caller-side `cmd.send + reply.wait` pairs so their
    /// reply slots never interleave. `CriticalSectionRawMutex` rather
    /// than `NoopRawMutex` so `Serial` is `Sync` and can live as a
    /// workspace `static`.
    caller: Mutex<CriticalSectionRawMutex, ()>,
    /// Held for the full drain-send-receive cycle of `read_register_raw` so
    /// two concurrent reads cannot interleave on the shared reply
    /// queue. Independent of `caller` because register reads and reply-
    /// bearing commands take different actor paths.
    register_session: Mutex<CriticalSectionRawMutex, ()>,
}

impl Serial {
    /// Settling window the chip's UART needs on either side of a baud-rate
    /// configure call so in-flight bytes don't get garbled.
    const BAUD_CHANGE_DRAIN: Duration = Duration::from_secs(1);

    /// Per-response wait inside `read_register_raw`. The chip answers a
    /// register read within a few hundred microseconds at fast baud; 50 ms
    /// is generous enough to absorb startup-rate dribble while bounding
    /// the worst-case stall on a missing chip.
    const READ_REPLY_TIMEOUT: Duration = Duration::from_millis(50);

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
            register_replies: Channel::new(),
            caller: Mutex::new(()),
            register_session: Mutex::new(()),
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

    /// Build and dispatch a register-write frame. Fire-and-forget; the
    /// chip does not acknowledge writes. Byte-level callers (the replay
    /// path, byte-frame tests) use this directly; consumers that hold a
    /// typed [`Register`] go through [`Self::write_register`].
    pub async fn write_register_raw(&self, addr: RawChipAddress, reg: u8, value: u32) {
        self.send_frame(build_write_register(addr, reg, value))
            .await;
    }

    /// Typed register-write wrapper. Serialises `reg` to its 4-byte
    /// wire payload and delegates to [`Self::write_register_raw`].
    /// Infallible because the bitfield storage is a plain `u32`.
    pub async fn write_register<R: Register>(&self, addr: RawChipAddress, reg: &R) {
        let value = u32::from_be_bytes(reg.to_be_bytes());
        self.write_register_raw(addr, R::REG_NUM, value).await;
    }

    /// Build and dispatch a register-read, then collect up to one reply
    /// per addressed chip. Holds [`Self::register_session`] across the
    /// drain-send-receive cycle so concurrent callers can't steal each
    /// other's replies. Stops early on the first per-reply timeout, so
    /// a missing chip caps the total stall at one
    /// [`Self::READ_REPLY_TIMEOUT`] window rather than `expected ×` it.
    pub async fn read_register_raw(
        &self,
        addr: RawChipAddress,
        reg: u8,
    ) -> HVec<RegisterResponse, MAX_CHIPS> {
        let _guard = self.register_session.lock().await;

        // Drop any stale replies that landed before we owned the session.
        while self.register_replies.try_receive().is_ok() {}

        self.send_frame(build_read_register(addr, reg)).await;

        let expected = match addr {
            RawChipAddress::All => usize::from(NUM_CHIPS),
            RawChipAddress::Chip(_) => 1,
        };

        let mut responses: HVec<RegisterResponse, MAX_CHIPS> = HVec::new();
        for _ in 0..expected {
            match with_timeout(Self::READ_REPLY_TIMEOUT, self.register_replies.receive()).await {
                Ok(resp) => {
                    let _ = responses.push(resp);
                }
                Err(_) => break,
            }
        }
        responses
    }

    /// Typed single-chip register-read wrapper. Issues a read at
    /// `RawChipAddress::Chip(chip)`, waits for the one expected reply,
    /// and decodes it into `R`. Returns `TimedOut` if the chip stays
    /// silent past [`Self::READ_REPLY_TIMEOUT`]. Broadcast reads stay on the
    /// raw path because the typed wrapper would have to drop chip-
    /// address pairing or wrap every entry, neither of which the
    /// `Tuner` / `Miner` callers need.
    pub async fn read_register<R: Register>(&self, chip: u8) -> Result<R, ErrorKind> {
        let raw = self
            .read_register_raw(RawChipAddress::Chip(chip), R::REG_NUM)
            .await;
        let response = raw.first().ok_or(ErrorKind::TimedOut)?;
        Ok(R::from_be_bytes(response.value.to_be_bytes()))
    }

    /// Server entry point. Owns the byte stream and the work registry;
    /// reads assignments from `session.work_receiver()` and submits
    /// found shares back through `session.submit_solution`. Bumps
    /// `ubos_metrics::hashrate::record_share` for every diff-1 chip
    /// share so the existing hashrate task picks them up.
    ///
    /// Returns `Ok(())` when a [`SerialMessage::Terminate`] command was
    /// honoured, and `Err(BrokenPipe)` only on serial EOF. Line-level
    /// read errors are absorbed inside `handle_read` so a transient
    /// FIFO overrun does not kill mining for the rest of the run.
    pub async fn run<R, T>(
        &self,
        mut rx: R,
        mut tx: T,
        session: &'static AsicSession,
    ) -> Result<(), ErrorKind>
    where
        R: Read + BaudRate,
        T: Write + BaudRate,
    {
        let mut work_receiver = session
            .work_receiver()
            .expect("BUG: AsicSession Watch must have a free receiver slot");
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
                    self.dispatch_work(&mut tx, &mut work_id, &mut registry, &mut work_receiver)
                        .await;
                }
                Either3::Second(read_result) => {
                    Self::handle_read(
                        read_result,
                        &mut parser,
                        &registry,
                        &self.register_replies,
                        session,
                    )?;
                }
                Either3::Third(msg) => {
                    if let ControlFlow::Break(()) = self
                        .handle_command(
                            msg,
                            &mut tx,
                            &mut parser,
                            &mut work_ticker,
                            &mut work_id,
                            &mut registry,
                        )
                        .await
                    {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn dispatch_work<T>(
        &self,
        tx: &mut T,
        work_id: &mut u8,
        registry: &mut [Option<WorkSlot>; REGISTRY_SIZE],
        work_receiver: &mut DynReceiver<'_, WorkAssignment>,
    ) where
        T: Write + BaudRate,
    {
        // Block until the stratum task publishes the first assignment;
        // afterwards `get` returns the latest immediately so subsequent
        // ticks pick up mid-job target / merkle-root changes without
        // blocking.
        let assignment = work_receiver.get().await;

        let hw_work = HwWork {
            work_id: *work_id,
            nonce_start: 0,
            nbits: assignment.nbits,
            ntime: assignment.ntime,
            merkle_root: assignment.merkle_root,
            prev_hash: assignment.prev_hash,
            version: assignment.version,
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
            assignment,
        });
        // `NUM_MIDSTATES` is 8; the cast is exact and fits well within u8.
        #[allow(clippy::cast_possible_truncation)]
        let stride = NUM_MIDSTATES as u8;
        *work_id = work_id.wrapping_add(stride);

        if let Err(e) = tx.write_all(&frame).await {
            error!("Failed to send work: {:?}", Debug2Format(&e.kind()));
        }
    }

    /// Returns `Err(BrokenPipe)` only on EOF, which is unrecoverable.
    /// Line-level read errors (overrun, framing, parity, break) are
    /// logged and swallowed: the embassy-rp `BufferedUart` re-arms its
    /// RX interrupt on the next read, and the response parser already
    /// resyncs on the next `0xAA 0x55` preamble plus CRC5. Treating
    /// them as fatal previously killed mining for the rest of the run
    /// after a single FIFO overrun, which is what happens on RP2350
    /// when probe-rs is attached and the host stalls the blocking
    /// defmt-rtt channel long enough for core 1's UART ISR to miss
    /// the 32-byte HW FIFO drain window at 3 Mbaud.
    fn handle_read<E: embedded_io::Error>(
        result: Result<usize, E>,
        parser: &mut ResponseParser,
        registry: &[Option<WorkSlot>; REGISTRY_SIZE],
        register_replies: &Channel<CriticalSectionRawMutex, RegisterResponse, MAX_CHIPS>,
        session: &'static AsicSession,
    ) -> Result<(), ErrorKind> {
        match result {
            Ok(0) => {
                error!("Serial port EOF");
                Err(ErrorKind::BrokenPipe)
            }
            Ok(n) => {
                trace!("RX ({} bytes): {}", n, HexBytes(&parser.free()[..n]));
                parser.commit(n);
                while let Some(resp) = parser.pop() {
                    Self::dispatch_response(&resp, registry, register_replies, session);
                }
                UART_DESYNC_BYTES.fetch_add(parser.take_desync(), Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                let kind = e.kind();
                warn!("Serial read error: {:?} (recovering)", Debug2Format(&kind));
                parser.clear();
                Ok(())
            }
        }
    }

    fn dispatch_response(
        resp: &Response,
        registry: &[Option<WorkSlot>; REGISTRY_SIZE],
        register_replies: &Channel<CriticalSectionRawMutex, RegisterResponse, MAX_CHIPS>,
        session: &'static AsicSession,
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

                if !hash_meets_target(&hash, &Self::DIFF_1_TARGET) {
                    warn!(
                        "nonce=0x{:08X} work_id={} ver_idx={} below diff 1 (hash={:?})",
                        wr.nonce,
                        wr.work_id,
                        wr.version_idx,
                        Debug2Format(&hash)
                    );
                    return;
                }

                // Credit one share against the target the chip was
                // scanning to so the shared hashrate EWMA reflects
                // the actual chip work rather than only pool-passing
                // shares. The estimator already weights by target.
                ubos_metrics::hashrate::record_share(&Self::DIFF_1_TARGET);

                if !hash_meets_target(&hash, &slot.assignment.target) {
                    trace!(
                        "nonce=0x{:08X} work_id={} ver_idx={} below pool target (hash={:?})",
                        wr.nonce,
                        wr.work_id,
                        wr.version_idx,
                        Debug2Format(&hash)
                    );
                    return;
                }

                info!(
                    "Submitting share: nonce=0x{:08X} work_id={} ver_idx={}",
                    wr.nonce, wr.work_id, wr.version_idx
                );

                let solution = Solution {
                    work_id: slot.assignment.work_id,
                    nonce: wr.nonce,
                    ntime: slot.assignment.ntime,
                    version_bits,
                };
                if !session.submit_solution(solution) {
                    warn!("AsicSession solutions full, dropping submission");
                }
            }
            Response::RegisterResponse(r) => {
                info!(
                    "RX register response: reg=0x{:02X} value=0x{:08X} chip=0x{:02X}",
                    r.reg, r.value, r.chip_addr
                );
                if register_replies.try_send(r.clone()).is_err() {
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
    ) -> ControlFlow<()>
    where
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
                Timer::after(Self::BAUD_CHANGE_DRAIN).await;
                let result = tx.set_baudrate(actual);
                info!("Serial baud rate changed to {}, settling 1s...", rate);
                Timer::after(Self::BAUD_CHANGE_DRAIN).await;
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
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

impl Default for Serial {
    fn default() -> Self {
        Self::new()
    }
}

/// Single workspace serial actor. One BM1362 chain per process; matches
/// `ubos_mining::psu::PSU` in shape so consumers reach in directly rather than
/// wiring a static through every task constructor.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub static SERIAL: Serial = Serial::new();
