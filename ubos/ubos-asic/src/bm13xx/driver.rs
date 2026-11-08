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

//! [`ChainDriver`] adapter for the Bitmain `BM13xx` family.
//!
//! Presents the register-level [`ChipProtocol`] recipe and [`framing`]
//! codec as the vendor-neutral [`ChainDriver`] the mining consumer
//! drives. The bring-up steps run over a [`TransportBus`] that frames the
//! recipe's register reads and writes onto a byte-level [`Transport`];
//! steady-state work and solutions go straight through the `HwWork`
//! serializer and the `ResponseParser`. No I/O and no global state lives
//! here — the driver owns only the chip model, the streaming parser, and
//! the desync tally the consumer drains through
//! [`ChainDriver::take_desync`].

use ubos_core::index::ChipAddress;
use ubos_core::units::Frequency;

use crate::HashboardDescriptor;
use crate::chain::{
    BIP320_VERSION_MASK, ChainDriver, ChainSolution, ChipDescriptor, ChipError, MAX_INGEST_CHUNK,
    MAX_SOLUTIONS_PER_BATCH, PollBatch, PollProgress, ResultMode, ShareTarget, SolutionBatch,
    WorkJob,
};
use crate::transport::Transport;

use super::chip::{Chip, ChipBus, ChipProtocol, MAX_CHAIN_CHIPS};
use super::framing::{
    ControlCommand, HwWork, RESPONSE_LEN, Response, ResponseParser, build_command,
    build_read_register, build_write_register,
};
use super::registers::{MidstateCalReg, Register, TicketMaskReg};

#[cfg(test)]
#[path = "tests/driver.rs"]
mod tests;

/// Share difficulty programmed at bring-up: 1 accepts every chip find so
/// the host sees the full nonce stream. Matches the former
/// `ubos_mining::consts::DIFFICULTY`.
const BRINGUP_DIFFICULTY: u32 = 1;

// A ticket mask exists only for a power-of-two difficulty, so
// `TicketMaskReg::from_difficulty` in `configure` returns `None`
// otherwise. Pin the invariant at compile time rather than leaning on
// the runtime `expect`.
const _: () = assert!(BRINGUP_DIFFICULTY.is_power_of_two());

/// Boot line rate the `BM13xx` chips listen on out of reset, in Hz.
const BOOT_UART_RATE: u32 = 115_200;

/// Settle window held on either side of the fast-UART baud switch so
/// in-flight bytes are not garbled. Matches the former
/// `Serial::BAUD_CHANGE_DRAIN` of one second.
const BAUD_DRAIN_MS: u32 = 1_000;

/// Per-read wait while collecting register replies during enumeration.
/// A missing chip caps the stall at one window; a broadcast read drains
/// the chain until the line goes quiet.
const READ_REPLY_TIMEOUT_MS: u32 = 50;

/// Bytes scanned per register read before giving up. Bounds the collect
/// loop when the line carries a steady byte stream that never parses to
/// a frame, e.g. garble after a failed baud switch; without a bound the
/// serial actor would never return to its park check. A full broadcast
/// from [`MAX_CHAIN_CHIPS`] chips is 32 * 11 = 352 bytes of 11-byte
/// replies; 1024 holds just under three of them.
const READ_REPLY_BUDGET: usize = 1_024;

// A full ingest chunk of RESPONSE_LEN-byte frames plus a carried-over
// partial must fit one batch, or `ingest` would drop a CRC-valid
// solution; this pins `MAX_SOLUTIONS_PER_BATCH` against
// `MAX_INGEST_CHUNK` so the two cannot drift apart.
const _: () = assert!(MAX_INGEST_CHUNK.div_ceil(RESPONSE_LEN) <= MAX_SOLUTIONS_PER_BATCH);

/// Bit offset of the BIP320 version-rolling field within the version
/// word. [`MidstateCalReg::version_rolled_bitmask`] holds the field
/// shifted down by this amount, one register bit per field bit.
const VERSION_FIELD_SHIFT: u32 = BIP320_VERSION_MASK.trailing_zeros();

/// [`ChainDriver`] for one Bitmain chip model. Holds the streaming
/// `ResponseParser` for the push solution path and a running desync
/// count drained through [`ChainDriver::take_desync`].
pub struct Bm13xxDriver {
    chip: Chip,
    parser: ResponseParser,
    desync: u32,
    /// The `version_rolled_bitmask` value last broadcast to the chain,
    /// or `None` before any bring-up has run. Lets `send_work` skip the
    /// register write when the job's mask has not changed since the
    /// last one.
    ///
    /// The register is one value for the whole chain while the mask is a
    /// property of each job, and the two disagree whenever a chain is
    /// fed from two pool slots whose grants differ. The chips keep a job
    /// resident in its work slot, so a job dispatched under a narrow
    /// mask keeps rolling after a later job widens the register, and its
    /// solutions come back with bits the consumer's per-job mask
    /// excludes. Those shares fail their own difficulty check and are
    /// dropped rather than mis-submitted, so the cost is lost shares on
    /// a pool switch and never a bad submission. Draining the in-flight
    /// work before widening is the fix and is not done: it needs a
    /// bounded drain the seam has no room for today.
    programmed_mask: Option<u16>,
}

impl Bm13xxDriver {
    /// Build a driver for `chip`.
    #[must_use]
    pub const fn new(chip: Chip) -> Self {
        Self {
            chip,
            parser: ResponseParser::new(),
            desync: 0,
            programmed_mask: None,
        }
    }
}

/// [`ChipBus`] over a byte-level [`Transport`]. Frames each register
/// read/write/command onto the wire and, for reads, buffers the reply
/// stream through a fresh [`ResponseParser`] until the line goes quiet.
/// Lives only for the duration of a bring-up step; bytes the ephemeral
/// parser discards — and any partial frame still buffered when the
/// collect loop stops — are folded into the driver's desync tally
/// through `desync`, so line corruption during bring-up reads still
/// reaches the metric.
struct TransportBus<'a, T: Transport> {
    io: &'a mut T,
    desync: &'a mut u32,
}

impl<T: Transport> ChipBus for TransportBus<'_, T> {
    async fn write_register_raw(&mut self, addr: ChipAddress, reg: u8, value: u32) {
        self.io
            .write_all(build_write_register(addr, reg, value).as_slice())
            .await;
    }

    async fn read_register<R: Register>(
        &mut self,
        chip_address: ChipAddress,
    ) -> heapless::Vec<(ChipAddress, R), MAX_CHAIN_CHIPS> {
        self.io
            .write_all(build_read_register(chip_address, R::REG_NUM).as_slice())
            .await;

        let mut parser = ResponseParser::new();
        let mut out = heapless::Vec::new();
        let mut buf = [0u8; 64];
        // Stop on the first quiet window: a single-chip read gets its one
        // reply then times out, a broadcast read drains every responder
        // then times out. The output buffer and the byte budget are the
        // other bounds. There is deliberately no pre-drain of stale line
        // bytes (the former actor discarded queued replies before each
        // read): the register filter below rejects a stale reply for any
        // other register, and a stale reply for this same register can
        // only come from a chip answering past its quiet window — which a
        // pre-drain could not distinguish from the fresh answer either —
        // so the drain would cost a timeout per read and filter nothing.
        let mut budget = READ_REPLY_BUDGET;
        'collect: loop {
            let n = self.io.read(&mut buf, READ_REPLY_TIMEOUT_MS).await;
            if n == 0 {
                break;
            }
            budget = budget.saturating_sub(n);
            parser.feed(&buf[..n]);
            // `pop` needs a chip only to decode work-response work-ids;
            // register replies never hit that path, so any model decodes
            // them identically and the value here is immaterial.
            //
            // The register filter drops a mismatched reply silently, by
            // design: without the pre-drain a stale reply is an expected
            // event, this crate has no logging seam, and folding it into
            // the desync tally would count well-formed frames as line
            // corruption. The former actor's mismatch trace and counter
            // (never exported) are deliberately not carried over. The
            // filter matches on the register address alone; every call
            // site today broadcasts, so a stale reply from the wrong
            // chip cannot occur, but a future unicast caller must also
            // filter on `reg_response.chip` matching the addressed
            // chip.
            while let Some(response) = parser.pop(Chip::Bm1362) {
                if let Response::RegisterResponse(reg_response) = response
                    && reg_response.reg == R::REG_NUM
                    && out
                        .push((
                            reg_response.chip,
                            R::from_be_bytes(reg_response.value.to_be_bytes()),
                        ))
                        .is_err()
                {
                    break 'collect;
                }
            }
            if budget == 0 {
                break;
            }
        }
        // The parser dies with this call; hand its desync count to the
        // driver so bring-up corruption reaches the metric. Bytes still
        // buffered belong to a frame that will now never complete —
        // every exit leaves them behind only anomalously (a partial
        // frame at the quiet window, a byte-budget stop mid-stream, an
        // over-full reply set) — so they count as desync too.
        // The parser buffers at most 256 bytes, so the count fits u32.
        #[allow(clippy::cast_possible_truncation)]
        let abandoned = parser.buffered() as u32;
        *self.desync = self
            .desync
            .wrapping_add(parser.take_desync())
            .wrapping_add(abandoned);
        out
    }

    async fn send_command(&mut self, command: ControlCommand) {
        self.io.write_all(build_command(command).as_slice()).await;
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.io.delay_ms(ms).await;
    }
}

impl ChainDriver for Bm13xxDriver {
    fn descriptor(&self) -> &'static ChipDescriptor {
        &self.chip.descriptor().chip
    }

    fn boot_uart_rate(&self) -> u32 {
        BOOT_UART_RATE
    }

    fn work_slots(&self) -> usize {
        // The chip echoes the work-id in one byte, strided by
        // midstate_count; the usable range is `(max_work_id + 1) /
        // midstate_count`. BM1362/BM1366: 256/8 = 32; BM1370: 128/16 = 8.
        (usize::from(self.chip.max_work_id()) + 1)
            / usize::from(self.chip.descriptor().midstate_count)
    }

    async fn reset_and_enumerate(
        &mut self,
        io: &mut impl Transport,
        _board: &HashboardDescriptor,
    ) -> u8 {
        let chip = self.chip;
        let mut bus = TransportBus {
            io,
            desync: &mut self.desync,
        };
        chip.reset_chain(&mut bus).await;
        // `reset_chain` broadcasts `MidstateCalReg::factory_default`,
        // whose bitmask is the full 0xFFFF; recording it here means a
        // job that grants the full BIP320 field causes no further
        // register write in `send_work`.
        self.programmed_mask = Some(MidstateCalReg::factory_default().version_rolled_bitmask());
        chip.enumerate_chips(&mut bus).await
    }

    async fn configure(
        &mut self,
        io: &mut impl Transport,
        board: &HashboardDescriptor,
    ) -> Result<Frequency, ChipError> {
        // `initialize_domains`, `initialize_cores`, and `split_nonce_space`
        // each walk `0..board.num_chips`, and enumeration only assigns
        // addresses below `MAX_CHAIN_CHIPS`. A descriptor claiming more
        // chips than the chain can address is a config bug.
        debug_assert!(
            usize::from(board.num_chips) <= MAX_CHAIN_CHIPS,
            "board.num_chips exceeds MAX_CHAIN_CHIPS"
        );
        let chip = self.chip;
        let safe_boot = chip.descriptor().chip.frequency_range.min();
        {
            let mut bus = TransportBus {
                io,
                desync: &mut self.desync,
            };
            chip.initialize_domains(&mut bus, board).await;
            chip.initialize_cores(&mut bus, board).await;
            chip.split_nonce_space(&mut bus, board).await;
            // PLL at the boot baud, then re-arm the cores against the now-
            // running hash clock. Matches the captured bring-up order.
            chip.set_pll(&mut bus, safe_boot).await?;
            chip.post_pll_rearm(&mut bus).await;
            let ticket = TicketMaskReg::from_difficulty(BRINGUP_DIFFICULTY)
                .expect("BUG: BRINGUP_DIFFICULTY must be a power of two");
            bus.write_register(ChipAddress::All, ticket).await;
            chip.enable_fast_uart(&mut bus).await;
        }
        // Baud switch last: drain, move the host line to the chip's fast
        // rate, settle. Then the counting number on the fast UART, which
        // the capture never sends at the boot baud. Bytes garbled across
        // the switch are left for the steady-state parser to resync past
        // (the former actor cleared its parser here instead). configure()
        // only writes after the switch, so those bytes surface in the
        // desync metric once, on the first steady-state ingest().
        // Drain before the settle rather than trusting the settle to do
        // it. A buffered transport acknowledges a write when it is
        // queued, so the delay alone measures nothing about what is still
        // on the line, and a switch over a non-empty buffer retimes the
        // queued tail to a rate the chips have not adopted.
        io.flush().await;
        io.delay_ms(BAUD_DRAIN_MS).await;
        let rate = chip.fast_uart_host_rate();
        io.set_baud(rate).map_err(|_| ChipError::BaudSwitch(rate))?;
        io.delay_ms(BAUD_DRAIN_MS).await;
        {
            let mut bus = TransportBus {
                io,
                desync: &mut self.desync,
            };
            chip.set_hcn(&mut bus, safe_boot).await?;
        }
        Ok(safe_boot)
    }

    async fn set_frequency(
        &mut self,
        io: &mut impl Transport,
        freq: Frequency,
    ) -> Result<(), ChipError> {
        let chip = self.chip;
        let mut bus = TransportBus {
            io,
            desync: &mut self.desync,
        };
        chip.set_hash_freq(&mut bus, freq).await
    }

    async fn send_work(&mut self, io: &mut impl Transport, job: &WorkJob) -> Result<(), ChipError> {
        // Map the neutral work_id onto the chip's strided work-id byte.
        // The consumer keeps work_id within work_slots, so the product
        // stays inside the chip's usable work-id range. A violation is
        // a caller bug, never a runtime condition like the ChipError
        // cases, so it panics instead of growing an error variant.
        assert!(
            (job.work_id as usize) < self.work_slots(),
            "BUG: work_id outside the chip's work-id range"
        );

        // The register holds the BIP320 field shifted down to its own
        // 16 bits; broadcast it only when the job actually narrows the
        // grant from what is already programmed.
        #[allow(clippy::cast_possible_truncation)]
        let wanted = ((job.version_mask & BIP320_VERSION_MASK) >> VERSION_FIELD_SHIFT) as u16;
        if self.programmed_mask != Some(wanted) {
            let mut bus = TransportBus {
                io,
                desync: &mut self.desync,
            };
            bus.write_register(
                ChipAddress::All,
                MidstateCalReg::factory_default().with_version_rolled_bitmask(wanted),
            )
            .await;
            self.programmed_mask = Some(wanted);
        }

        let midstate = self.chip.descriptor().midstate_count;
        #[allow(clippy::cast_possible_truncation)]
        let work_id = (job.work_id as u8).wrapping_mul(midstate);
        let hw_work = HwWork {
            work_id,
            nonce_start: job.nonce_start,
            nbits: job.nbits,
            ntime: job.ntime,
            merkle_root: job.merkle_root,
            prev_hash: job.prev_hash,
            version: job.version,
        };
        io.write_all(&hw_work.serialize()).await;
        Ok(())
    }

    fn result_mode(&self) -> ResultMode {
        ResultMode::Push
    }

    fn difficulty_target(&self) -> ShareTarget {
        // The difficulty `configure` programs into the ticket mask, so
        // the register and the reported difficulty cannot drift apart.
        ShareTarget::from_pow2_difficulty(u64::from(BRINGUP_DIFFICULTY))
    }

    fn ingest(&mut self, bytes: &[u8]) -> SolutionBatch {
        let midstate = self.chip.descriptor().midstate_count;
        let mut out = SolutionBatch::new();
        let mut remaining = bytes;
        // Mirrors `feed_parser` deliberately instead of calling it:
        // this loop pops frames between fills, so a full parser drains
        // through decode, where `feed_parser` fills without popping and
        // leaves the drain to its caller.
        while !remaining.is_empty() {
            let free = self.parser.free();
            if free.is_empty() {
                self.parser.clear();
                continue;
            }
            let n = free.len().min(remaining.len());
            free[..n].copy_from_slice(&remaining[..n]);
            self.parser.commit(n);
            remaining = &remaining[n..];
            while let Some(response) = self.parser.pop(self.chip) {
                if let Response::HwSolution(solution) = response {
                    // pop() already decoded the work-id; recover the hardware
                    // work ID by undoing the midstate stride.
                    let sol = ChainSolution {
                        work_id: u32::from(solution.work_id / midstate),
                        nonce: solution.nonce,
                        ntime_offset: 0,
                        version_bits: solution.version_bits(),
                    };
                    if out.push(sol).is_err() {
                        // Batch full. `sol` is an already CRC-validated
                        // solution, and it is genuinely lost here: it is
                        // never returned to the caller and never
                        // resubmitted. This path is unreachable today
                        // because a single `ingest` call carries at most
                        // one read chunk, and that plus the parser's
                        // partial-frame carry-over decodes at most
                        // `MAX_SOLUTIONS_PER_BATCH` frames — but the trait
                        // puts no bound on `bytes`, so a caller feeding a
                        // larger backlog could reach it and lose a
                        // solution. The lost solution's frame length and
                        // the unfed remainder are folded into the desync
                        // tally so the loss at least surfaces there.
                        #[allow(clippy::cast_possible_truncation)]
                        let dropped = (RESPONSE_LEN + remaining.len()) as u32;
                        self.desync = self
                            .desync
                            .wrapping_add(dropped)
                            .wrapping_add(self.parser.take_desync());
                        return out;
                    }
                }
            }
        }
        self.desync = self.desync.wrapping_add(self.parser.take_desync());
        out
    }

    async fn poll_results(&mut self, _io: &mut impl Transport) -> PollBatch {
        PollBatch {
            solutions: SolutionBatch::new(),
            difficulty: self.difficulty_target(),
            progress: PollProgress::Complete,
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.chip);
    }

    fn line_error(&mut self) {
        // Drop the buffered partial frame so it cannot splice with the
        // bytes that follow the fault; `clear` counts the drop as desync
        // and the tally moves to the driver for the consumer to drain.
        self.parser.clear();
        self.desync = self.desync.wrapping_add(self.parser.take_desync());
    }

    fn take_desync(&mut self) -> u32 {
        core::mem::take(&mut self.desync)
    }
}
