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

//! Vendor-neutral chain-driver seam.
//!
//! [`ChainDriver`] is the operations contract every ASIC vendor
//! implements: bring the chain up to a safe boot point, set the hash
//! frequency, dispatch a job, and collect solutions. It is sans-IO — each
//! method drives a [`Transport`] the consumer owns and holds no UART — so
//! the whole driver is unit-testable against a mock transport, exactly as
//! the register-level `ChipProtocol` was.
//!
//! The seam sits above framing on purpose. Every supported chip family
//! shares the shape *register read/write over a daisy-chained UART with
//! broadcast addressing, enumerate-at-low-baud then switch to a fast
//! baud* but disagrees on everything below it: preamble, CRC, address
//! width, the work frame, and — critically — how solutions arrive. Some
//! families push nonce frames unsolicited; others poll a nonce FIFO.
//! [`ResultMode`] lets the consumer branch its steady-state loop on that
//! difference while everything else stays neutral.
//!
//! `ChainDriver` has async methods and is not dyn-compatible, so a
//! consumer selects and holds its concrete driver type directly (by
//! generic parameter or a per-vendor dispatch enum of its own) rather
//! than through a trait object.

use heapless::Vec;
use sha2::{Digest, Sha256};
use ubos_core::units::{Frequency, FrequencyRange, Voltage};

use crate::board::HashboardDescriptor;
use crate::transport::Transport;

#[cfg(test)]
#[path = "tests/chain.rs"]
mod tests;

/// BIP320 version-rolling field mask: bits 28:13 carry the rolled value.
pub const BIP320_VERSION_MASK: u32 = 0x1FFF_E000;

/// Merge the rolled version bits from a solution into a base version,
/// replacing only the BIP320 field. `version_bits` is already masked to
/// that field by the driver.
///
/// The merge always spans the whole field, whatever
/// [`WorkJob::version_mask`] granted: a compliant driver echoes only
/// bits it was allowed to roll, so the wider merge is exact for it,
/// and narrowing the merge to the grant would need the grant threaded
/// to every caller before any driver misbehaves enough to matter.
#[must_use]
pub fn rolled_version(base_version: u32, version_bits: u32) -> u32 {
    (base_version & !BIP320_VERSION_MASK) | (version_bits & BIP320_VERSION_MASK)
}

/// Assemble the canonical 80-byte block header. `prev_hash` and
/// `merkle_root` go in the caller's canonical internal order without
/// further byte-swapping; the stratum adapter already word-swaps
/// `prev_hash` on arrival. The single header layout both the share
/// verification and any chip family's work frame build from.
#[must_use]
pub fn block_header(
    version: u32,
    prev_hash: &[u8; 32],
    merkle_root: &[u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 80] {
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&version.to_le_bytes());
    header[4..36].copy_from_slice(prev_hash);
    header[36..68].copy_from_slice(merkle_root);
    header[68..72].copy_from_slice(&ntime.to_le_bytes());
    header[72..76].copy_from_slice(&nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

/// Double-SHA256 of the 80-byte block header for share verification.
///
/// Vendor-neutral, and byte-identical to the header any chip family's
/// driver hashes. Returns the digest in native little-endian order.
#[must_use]
pub fn solution_hash(
    version: u32,
    prev_hash: &[u8; 32],
    merkle_root: &[u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 32] {
    let header = block_header(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let first = Sha256::digest(header);
    let second = Sha256::digest(first);
    second.into()
}

/// Failure surface of a [`ChainDriver`] operation. Register reads inside
/// a bring-up return an empty list rather than an error, so the fallible
/// steps are programming a frequency outside the chip's range and moving
/// the host line to the fast rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipError {
    /// The requested frequency is outside the chip's range.
    UnsupportedFrequency(Frequency),
    /// The host UART refused the switch to the given rate, in Hz. The
    /// chips have already moved, so the chain must be reset and brought
    /// up again rather than mined on a misclocked line.
    BaudSwitch(u32),
}

// Hand-written so the error can carry a `Frequency` without `ubos-core`
// taking a defmt dependency; the frequency is logged in kHz.
#[cfg(feature = "defmt")]
impl defmt::Format for ChipError {
    fn format(&self, fmt: defmt::Formatter) {
        match self {
            ChipError::UnsupportedFrequency(freq) => {
                defmt::write!(fmt, "UnsupportedFrequency({} kHz)", freq.khz());
            }
            ChipError::BaudSwitch(hz) => {
                defmt::write!(fmt, "BaudSwitch({} Hz)", hz);
            }
        }
    }
}

/// Capability bounds for a single ASIC, consumed by the tuner. Strictly
/// vendor-neutral — each chip family fills it from its own static
/// descriptors; vendor-specific constants such as Bitmain's chip-id,
/// clock delay, and work-id stride stay in that family's module beside
/// its driver.
#[derive(Clone, Copy)]
pub struct ChipDescriptor {
    /// Model string reported to the REST API and dashboard.
    pub asic_model: &'static str,
    /// Total hashing cores on this chip model. Identity and capability
    /// metadata; not a register value.
    pub core_count: u32,
    /// Maximum voltage a single whole chip tolerates. Hashboard-local
    /// safety policy, not a chip-spec value. The hashboard-rail ceiling
    /// consumed by the tuner is this value scaled by the hashboard's
    /// `num_voltage_domains`.
    pub max_chip_voltage: Voltage,
    /// PLL hash-clock resolution: the frequency grid every programmed
    /// operating point lands on. The tuner ramps on and floors targets to
    /// this grid so the requested frequency equals the realized hash
    /// clock.
    pub frequency_step: Frequency,
    /// Inclusive hash-clock frequency envelope the chip operates within.
    /// The tuner ramps inside it and the register calculators reject
    /// targets outside it.
    pub frequency_range: FrequencyRange,
}

/// Upper bound on bytes a consumer feeds to a single
/// [`ChainDriver::ingest`] call. Part of the seam contract:
/// [`MAX_SOLUTIONS_PER_BATCH`] is sized so a push driver decodes a full
/// chunk of its smallest frames plus a carried-over partial without
/// overflowing the batch, so a consumer that sizes its UART read buffer
/// to this bound never loses a decoded solution to batch overflow.
pub const MAX_INGEST_CHUNK: usize = 256;

/// Upper bound on solutions a single [`ChainDriver::poll_results`] or
/// [`ChainDriver::ingest`] call returns. Sized to the Bitmain push
/// path's worst case: a [`MAX_INGEST_CHUNK`]-byte read plus the parser's
/// partial-frame carry-over decodes at most 24 of its 11-byte frames, a
/// bound the driver pins with a compile-time assertion beside its frame
/// length. A polled nonce burst from a smaller-frame family fits
/// comfortably within the same bound. The consumer drains the returned
/// batch before the next call.
pub const MAX_SOLUTIONS_PER_BATCH: usize = 24;

/// How a chip family delivers found nonces, so the consumer can shape its
/// steady-state loop correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ResultMode {
    /// The chip pushes solution frames unsolicited (Bitmain). The
    /// consumer feeds freshly-read UART bytes to [`ChainDriver::ingest`].
    Push,
    /// The host must read a nonce FIFO to collect solutions. The
    /// consumer calls [`ChainDriver::poll_results`] on a ticker.
    Poll,
}

/// A vendor-neutral mining job handed to [`ChainDriver::send_work`].
///
/// Header fields are in canonical Bitcoin internal byte order, matching
/// the former `HwWork`; the stratum adapter word-swaps on arrival.
/// `job_key` is the consumer-assigned correlation token: the driver
/// encodes it into the wire frame in whatever vendor-specific way the
/// chip echoes back, and returns it in [`ChainSolution::job_key`] so the
/// consumer can recover the originating assignment. The consumer keeps
/// `job_key` within the range the chip's echo field can carry; the driver
/// owns the mapping to and from the on-wire id.
#[derive(Debug, Clone)]
pub struct WorkJob {
    /// Correlation token echoed back in [`ChainSolution::job_key`].
    pub job_key: u32,
    /// Starting nonce value for this work item.
    pub nonce_start: u32,
    /// Encoded difficulty target (`nBits` field).
    pub nbits: u32,
    /// Block timestamp.
    pub ntime: u32,
    /// Block version field, may be rolled by the chip.
    pub version: u32,
    /// Mask of version bits the chip may roll; zero pins the version.
    ///
    /// Only the drivers that opt in act on this; a driver that ignores
    /// it keeps its family's fixed rolling behaviour, and the
    /// reconstruction in [`rolled_version`] still assumes the full
    /// BIP320 field either way. A pool that pins the version is
    /// honoured by a driver that reads the mask, not by the seam.
    pub version_mask: u32,
    /// Merkle root in canonical Bitcoin internal byte order.
    pub merkle_root: [u8; 32],
    /// Previous block hash in canonical Bitcoin internal byte order.
    pub prev_hash: [u8; 32],
}

/// A found nonce a [`ChainDriver`] decoded off the chain.
///
/// The consumer maps `job_key` back to the originating assignment,
/// reconstructs the header, re-verifies the share against target, and
/// submits it. The driver reports only what the chip returned; the base
/// header fields come from the consumer's registry.
#[derive(Debug, Clone, Copy)]
pub struct ChainSolution {
    /// Correlation token from the originating [`WorkJob::job_key`].
    pub job_key: u32,
    /// Nonce value found by the chip.
    pub nonce: u32,
    /// Seconds added to the job's `ntime` by a chip that rolls the
    /// timestamp. Zero for chips that do not roll ntime (Bitmain); the
    /// consumer hashes and submits `job.ntime + ntime_offset`.
    pub ntime_offset: u32,
    /// BIP320 version-rolling bits, masked to the rolling field. The base
    /// version is re-merged at submit time.
    pub version_bits: u32,
}

/// A batch of solutions returned by a single collection call.
pub type SolutionBatch = Vec<ChainSolution, MAX_SOLUTIONS_PER_BATCH>;

/// A 256-bit share target in little-endian byte order.
///
/// Wraps the raw bytes the chip's hardware filter and the consumer's
/// floor check both compare against, so the constant lives in one
/// place and the type documents intent at the trait boundary.
pub struct ShareTarget([u8; 32]);

impl ShareTarget {
    /// Difficulty one is `DIFF_1_MANTISSA * 2^DIFF_1_EXPONENT`, and a
    /// power-of-two difficulty only shifts the mantissa down, so the
    /// pair generates every target this type constructs.
    const DIFF_1_MANTISSA: u32 = 0xFFFF;
    /// The exponent difficulty one places the mantissa at; see
    /// [`Self::DIFF_1_MANTISSA`].
    const DIFF_1_EXPONENT: u32 = 208;

    /// Difficulty one: Bitcoin's mainnet pow limit, the loosest filter
    /// a chip ships with.
    pub const DIFF_1: Self = Self::from_pow2_difficulty(1);

    /// The target whose shares each count for `difficulty`
    /// difficulty-one shares: [`Self::DIFF_1`] divided by `difficulty`.
    ///
    /// # Panics
    ///
    /// When `difficulty` is not a power of two, because only a power
    /// of two divides the mantissa as an exact shift.
    #[must_use]
    pub const fn from_pow2_difficulty(difficulty: u64) -> Self {
        assert!(difficulty.is_power_of_two());
        let exponent = Self::DIFF_1_EXPONENT - difficulty.trailing_zeros();
        let mantissa = (Self::DIFF_1_MANTISSA << (exponent % 8)).to_le_bytes();
        let start = (exponent / 8) as usize;
        let mut bytes = [0u8; 32];
        let mut offset = 0;
        while offset < mantissa.len() {
            bytes[start + offset] = mantissa[offset];
            offset += 1;
        }
        Self(bytes)
    }

    /// The raw little-endian bytes, callable in const context where
    /// the `Deref` impl is not.
    #[must_use]
    pub const fn to_le_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl core::ops::Deref for ShareTarget {
    type Target = [u8; 32];

    fn deref(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The vendor-neutral operations a mining consumer drives, one impl per
/// chip family. Sans-IO: every method that touches the wire takes a
/// `&mut impl Transport`; the driver owns no UART and no global state.
///
/// Lifecycle the consumer runs: [`Self::reset_and_enumerate`] then
/// [`Self::configure`] once from the post-reset baseline, then a ramp
/// of [`Self::set_frequency`] calls,
/// then steady state — [`Self::send_work`] on a ticker plus solution
/// collection via [`Self::ingest`] or [`Self::poll_results`] according to
/// [`Self::result_mode`].
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu` and the former `ChipProtocol`.
#[allow(async_fn_in_trait)]
pub trait ChainDriver {
    /// Static capability and identity constants for this chip model.
    fn descriptor(&self) -> &'static ChipDescriptor;

    /// Line rate, in Hz, the chips listen on out of reset. The consumer
    /// arms its UART at this rate before [`Self::reset_and_enumerate`];
    /// [`Self::configure`] switches to the fast rate itself.
    fn boot_uart_rate(&self) -> u32;

    /// Number of concurrent jobs the consumer may keep in flight, i.e.
    /// the valid range of [`WorkJob::job_key`] (`0..job_slots`). The
    /// driver maps each `job_key` to and from the chip's on-wire echo
    /// field, so the consumer sizes its work registry to this and never
    /// touches the wire id. An implementation returns at least 1; the
    /// consumer sizes its work registry to this value and asserts the
    /// lower bound at bring-up rather than trusting a registry of zero
    /// slots.
    fn job_slots(&self) -> usize;

    /// Reset the chain to a known state and enumerate it, at the boot
    /// baud. Returns the number of chips that answered. Idempotent and
    /// baud-preserving so the consumer can retry it on a count mismatch
    /// before committing to [`Self::configure`]. A family whose reset
    /// needs board-level control that [`Transport`] does not carry
    /// documents that step on its driver; the consumer's retry repeats
    /// only the wire sequence.
    async fn reset_and_enumerate(
        &mut self,
        io: &mut impl Transport,
        board: &HashboardDescriptor,
    ) -> u8;

    /// Configure the enumerated chain and bring it to a running,
    /// safe-boot operating point: per-domain/core setup, the boot-
    /// frequency PLL, the difficulty-1 share filter, and the switch to
    /// the fast UART — including switching `io` itself to the fast rate.
    /// On return the chain is hashing at the descriptor's minimum
    /// frequency and the line is at the fast rate.
    async fn configure(
        &mut self,
        io: &mut impl Transport,
        board: &HashboardDescriptor,
    ) -> Result<(), ChipError>;

    /// Program the hash-clock frequency. Called repeatedly by the tuner
    /// ramp; the driver owns the vendor-specific PLL programming.
    async fn set_frequency(
        &mut self,
        io: &mut impl Transport,
        freq: Frequency,
    ) -> Result<(), ChipError>;

    /// Dispatch one job to the chain.
    ///
    /// Not a pure serializer: some chips must write auxiliary registers
    /// before the work frame, such as a version-rolling mask broadcast
    /// when it changes, so this drives the transport directly.
    async fn send_work(&mut self, io: &mut impl Transport, job: &WorkJob) -> Result<(), ChipError>;

    /// How this chip family delivers solutions. Drives whether the
    /// consumer calls [`Self::ingest`] (Push) or [`Self::poll_results`]
    /// (Poll).
    fn result_mode(&self) -> ResultMode;

    /// The share target the chip's own hardware filter enforces.
    /// Every solution the driver returns meets it, so the consumer
    /// verifies against it as the floor and credits each solution at
    /// its weight; a floor above difficulty one is how a chip family
    /// keeps its result path from flooding a slow link.
    fn floor_target(&self) -> ShareTarget;

    /// Push path: feed bytes freshly read from the line and return any
    /// complete solutions decoded from them. Stateful — the driver
    /// buffers partial frames across calls. A no-op for Poll drivers.
    fn ingest(&mut self, bytes: &[u8]) -> SolutionBatch;

    /// Poll path: read the chip's nonce FIFO over `io` and return the
    /// decoded solutions. A no-op (empty batch) for Push drivers.
    async fn poll_results(&mut self, io: &mut impl Transport) -> SolutionBatch;

    /// Reset the driver to its freshly-constructed state: clear the
    /// streaming parser, any cached masks, and the desync tally, keeping
    /// the chip identity. The consumer calls this when re-arming the
    /// chain, e.g. on a `Reset` command, before re-running bring-up. A
    /// caller that wants the pre-reset desync count must call
    /// [`Self::take_desync`] before this, since `reset` clears it.
    fn reset(&mut self);

    /// Reset the streaming solution parser after a line-level fault
    /// (overrun, framing, parity error). A frame truncated by the fault
    /// must not splice with post-fault bytes into a frame that passes
    /// the checksum; dropping the buffered partial bounds the damage to
    /// the frame the fault already corrupted. Discarded bytes count as
    /// desync. Defaults to a no-op for drivers without parser state.
    fn line_error(&mut self) {}

    /// Bytes discarded resyncing to a frame boundary or on a failed CRC
    /// since the last call, drained to zero. Telemetry the consumer may
    /// forward to a metric; defaults to zero for families that do not
    /// track it.
    fn take_desync(&mut self) -> u32 {
        0
    }
}
