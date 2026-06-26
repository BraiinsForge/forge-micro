//! BM1362 frame and parser primitives.
//!
//! Owns the byte layouts the chip understands on the serial line: command
//! frames the host emits ([`Frame`] + builders), the work-frame format
//! ([`HwWork`]) the chip receives, and the parser that pulls register
//! responses and hardware solutions out of a streaming byte buffer
//! ([`ResponseParser`]).

use heapless::Vec as HVec;
use sha2::{Digest, Sha256};

use crate::chip::Chip;

#[cfg(test)]
#[path = "tests/framing.rs"]
mod tests;

// ---------------------------------------------------------------------------
// CRC
// ---------------------------------------------------------------------------

/// CRC5 computation for BM1362 serial protocol.
///
/// - `standard=true, initial=0x1F`: request commands (host -> chip)
/// - `standard=false, initial=0x03`: register responses
/// - `standard=false, initial=0x06`: work responses
fn crc5(message: &[u8], initial: u8, standard: bool) -> u8 {
    const POLY: u8 = 0x05;
    let mut crc = initial & 0x1F;
    for &orig_byte in message {
        let mut byte = orig_byte;
        for _ in 0..8 {
            if ((byte >> 3) ^ crc) & 0x10 != 0 {
                crc = ((crc << 1) ^ POLY) & 0x1F;
            } else {
                crc = (crc << 1) & 0x1F;
            }
            if !standard && (byte >> 3) & 0x10 != 0 {
                crc ^= 0x08;
            }
            byte <<= 1;
        }
    }
    crc
}

/// CRC16-CCITT-FALSE (poly 0x1021, init 0xFFFF) for work frames.
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// HwSolution / HwWork
// ---------------------------------------------------------------------------

/// A hardware solution returned by the chip for an outstanding work item.
#[derive(Debug, Clone)]
pub struct HwSolution {
    /// Nonce value found by the chip.
    pub nonce: u32,
    /// Byte 6 of the response frame. The chip's meaning for this byte is
    /// undocumented and unused, so it stays underscore-prefixed to
    /// acknowledge the slot without tripping dead-code lints.
    _extra_diff: u8,
    /// Work-item identifier matching the dispatched `HwWork::work_id`.
    pub work_id: u8,
    /// BIP320 version-rolling index.
    pub version_idx: u16,
}

impl HwSolution {
    /// Bitmask selecting the version-rolling bits in the block version
    /// field per BIP320. Bits 29:13 carry the rolled value.
    const BIP320_VERSION_MASK: u32 = 0x1FFF_E000;

    /// Bit offset of the BIP320 version-rolling field within the 32-bit
    /// block version.
    const BIP320_VERSION_SHIFT: u32 = 13;

    /// Compute the rolled block version given a base version.
    fn rolled_version(&self, base_version: u32) -> u32 {
        let base = base_version & !Self::BIP320_VERSION_MASK;
        base | ((u32::from(self.version_idx) << Self::BIP320_VERSION_SHIFT)
            & Self::BIP320_VERSION_MASK)
    }
}

/// Reverse the bytes within each 4-byte word of a 256-bit value.
///
/// Stratum's `mining.notify` emits `prev_hash` as eight big-endian 32-bit
/// words, but Bitcoin's block header wants them in native (little-endian)
/// byte order inside each word. `mining_protocol::hashing::build_header`
/// applies this swap inline; ubos-mining stores `HwWork.prev_hash` in canonical
/// post-swap form so both the local hash check and the chip wire frame
/// see the same bytes.
#[cfg(test)]
#[must_use]
fn word_swap_prev_hash(input: &[u8; 32]) -> [u8; 32] {
    let mut output = [0u8; 32];
    for word_index in 0..8 {
        let offset = word_index * 4;
        output[offset] = input[offset + 3];
        output[offset + 1] = input[offset + 2];
        output[offset + 2] = input[offset + 1];
        output[offset + 3] = input[offset];
    }
    output
}

/// A work assignment to send to the chip.
///
/// `prev_hash` is in canonical Bitcoin internal byte order (the stratum
/// adapter word-swaps on arrival). Callers copy it straight from the
/// `WorkAssignment` without any additional transformation.
#[derive(Debug, Clone)]
pub struct HwWork {
    /// Work-item identifier sent to the chip and echoed back in `HwSolution`.
    pub work_id: u8,
    /// Starting nonce value for this work item.
    pub nonce_start: u32,
    /// Encoded difficulty target (`nBits` field).
    pub nbits: u32,
    /// Block timestamp.
    pub ntime: u32,
    /// Merkle root in canonical Bitcoin internal byte order.
    pub merkle_root: [u8; 32],
    /// Previous block hash in canonical Bitcoin internal byte order.
    pub prev_hash: [u8; 32],
    /// Block version field, may be rolled by the chip.
    pub version: u32,
}

impl HwWork {
    /// Build the 80-byte block header for hash verification.
    fn block_header(&self, nonce: u32, rolled_version: u32) -> [u8; 80] {
        let mut header = [0u8; 80];
        header[0..4].copy_from_slice(&rolled_version.to_le_bytes());
        header[4..36].copy_from_slice(&self.prev_hash);
        header[36..68].copy_from_slice(&self.merkle_root);
        header[68..72].copy_from_slice(&self.ntime.to_le_bytes());
        header[72..76].copy_from_slice(&self.nbits.to_le_bytes());
        header[76..80].copy_from_slice(&nonce.to_le_bytes());
        header
    }

    /// Compute double-SHA256 hash for a work response. Returns the 32-byte
    /// digest in native (little-endian) byte order.
    #[must_use]
    pub fn compute_hash(&self, response: &HwSolution) -> [u8; 32] {
        let version = response.rolled_version(self.version);
        let header = self.block_header(response.nonce, version);
        let first = Sha256::digest(header);
        let second = Sha256::digest(first);
        second.into()
    }

    /// Serialize into the 88-byte on-wire frame.
    #[must_use]
    pub fn serialize(&self) -> [u8; 88] {
        let mut buffer = [0u8; 88];
        buffer[0] = 0x55;
        buffer[1] = 0xAA;
        buffer[2] = 0x21; // work command
        buffer[3] = 0x36; // length = 54
        buffer[4] = self.work_id;
        buffer[5] = 0x01; // multiversion = 1

        buffer[6..10].copy_from_slice(&self.nonce_start.to_le_bytes());
        buffer[10..14].copy_from_slice(&self.nbits.to_le_bytes());
        buffer[14..18].copy_from_slice(&self.ntime.to_le_bytes());

        // merkle_root: reverse 32 bytes then swap each 4-byte chunk to LE
        let transformed_merkle = transform_hash(&self.merkle_root);
        buffer[18..50].copy_from_slice(&transformed_merkle);

        // prev_hash: same transformation
        let transformed_prev = transform_hash(&self.prev_hash);
        buffer[50..82].copy_from_slice(&transformed_prev);

        buffer[82..86].copy_from_slice(&self.version.to_le_bytes());

        // CRC16-CCITT over bytes 2..86
        let crc = crc16_ccitt(&buffer[2..86]);
        buffer[86..88].copy_from_slice(&crc.to_be_bytes());

        buffer
    }
}

/// Transform a 32-byte hash for `HwWork` frame:
/// 1. Reverse the entire 32 bytes
/// 2. Swap bytes within each 4-byte chunk (u32 LE conversion per chunk)
fn transform_hash(hash: &[u8; 32]) -> [u8; 32] {
    let mut reversed = *hash;
    reversed.reverse();

    let mut result = [0u8; 32];
    for index in 0..8 {
        let offset = index * 4;
        // Swap bytes within each 4-byte chunk (to LE).
        result[offset] = reversed[offset + 3];
        result[offset + 1] = reversed[offset + 2];
        result[offset + 2] = reversed[offset + 1];
        result[offset + 3] = reversed[offset];
    }
    result
}

// ---------------------------------------------------------------------------
// Frame + command builders
// ---------------------------------------------------------------------------

const HEADER_TX: [u8; 2] = [0x55, 0xAA];
const RESPONSE_LEN: usize = 11;
const CRC5_INIT_REG_RESPONSE: u8 = 0x03;
const CRC5_INIT_WORK_RESPONSE: u8 = 0x06;

/// An outbound command frame on the wire. Fits the three non-work command
/// shapes the host ever sends: read register (7), write register (11), and
/// raw command with at most 10 payload bytes (at most 13). The fixed 16-byte
/// cap keeps the buffer on the stack.
#[derive(Debug, Clone)]
pub struct Frame(HVec<u8, 16>);

impl Frame {
    fn new() -> Self {
        Frame(HVec::new())
    }

    /// Return the serialized frame bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

// Command type field: VilCtlCmd = 0x02, shifted to bits [7:5].
const CMD_TYPE: u8 = 0x02 << 5; // = 0x40
const TO_ALL: u8 = 0x10;

// Command codes used by the register builders. The register-less control
// codes (SetChipAddress, ChainInactive) live in [`ControlCode`] below and
// reach the wire via `build_command`.
const CODE_SET_CONFIG: u8 = 1;
const CODE_GET_STATUS: u8 = 2;

/// VIL control command code (3-bit field in the cmd byte). Register
/// reads and writes use `SetConfig`/`GetStatus` codes that have their
/// own builders; this enum covers the register-less control commands
/// the host issues during chain bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum ControlCode {
    /// Assign an address to the next chip on the chain that has not
    /// claimed one yet.
    SetChipAddress = 0,
    /// Tell every chip on the chain to drop its assigned address.
    ChainInactive = 3,
}

impl ControlCode {
    /// Numeric code packed into the low bits of the cmd byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Register-less VIL control command. Carries the protocol code,
/// the broadcast flag, and the hw-addr the chip-side decoder
/// consults. Construct via [`Self::chain_inactive`] or
/// [`Self::set_chip_address`] and hand to the wire encoder; the
/// encoder is the only place that knows the preamble + CRC framing.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ControlCommand {
    code: ControlCode,
    broadcast: bool,
    hw_addr: u8,
}

impl ControlCommand {
    /// Broadcast command that tells every chip on the chain to drop
    /// its assigned address. Issued three times at bring-up to
    /// tolerate any single-frame loss before address re-assignment
    /// begins.
    #[must_use]
    pub const fn chain_inactive() -> Self {
        Self {
            code: ControlCode::ChainInactive,
            broadcast: true,
            hw_addr: 0x00,
        }
    }

    /// Unicast command that assigns `hw_addr` to the first chip on
    /// the chain without a claimed address. Broadcasting would set
    /// every chip to the same address; the caller walks one
    /// `set_chip_address` per chip slot instead.
    #[must_use]
    pub const fn set_chip_address(hw_addr: u8) -> Self {
        Self {
            code: ControlCode::SetChipAddress,
            broadcast: false,
            hw_addr,
        }
    }

    /// Protocol code carried in the low bits of the cmd byte.
    #[must_use]
    pub const fn code(&self) -> ControlCode {
        self.code
    }

    /// `true` if the cmd byte must set the broadcast bit.
    #[must_use]
    pub const fn is_broadcast(&self) -> bool {
        self.broadcast
    }

    /// hw-addr the chip-side decoder consults; `0x00` for broadcast.
    #[must_use]
    pub const fn hw_addr(&self) -> u8 {
        self.hw_addr
    }
}

/// A chip address on the serial chain: either broadcast to all chips or
/// unicast to the chip at the given hardware address.
#[derive(Debug, Clone, Copy)]
pub enum ChipAddress {
    /// Address all chips on the chain simultaneously.
    All,
    /// Address the chip at the given hardware address byte.
    Chip(u8),
}

impl ChipAddress {
    fn hw_addr(self) -> u8 {
        match self {
            ChipAddress::All => 0x00,
            ChipAddress::Chip(index) => index,
        }
    }

    fn is_broadcast(self) -> bool {
        matches!(self, ChipAddress::All)
    }
}

// The longest command this module builds is 11 bytes (2 preamble +
// 8 payload + 1 CRC for a write-register frame); Frame is 16 bytes.
// All in-module callers respect that bound.
fn push_frame(frame: &mut Frame, header: &[u8], payload: &[u8], crc: u8) {
    frame
        .0
        .extend_from_slice(header)
        .expect("BUG: Frame capacity exceeded");
    frame
        .0
        .extend_from_slice(payload)
        .expect("BUG: Frame capacity exceeded");
    frame.0.push(crc).expect("BUG: Frame capacity exceeded");
}

/// Build a write register command (`SetConfig`) — 11 bytes on wire.
#[must_use]
pub fn build_write_register(addr: ChipAddress, reg: u8, value: u32) -> Frame {
    let cmd = CMD_TYPE | if addr.is_broadcast() { TO_ALL } else { 0 } | CODE_SET_CONFIG;
    let len = 9u8; // packed_size=8 + 1 CRC
    let hw_addr = addr.hw_addr();
    let value_bytes = value.to_be_bytes();

    let payload = [
        cmd,
        len,
        hw_addr,
        reg,
        value_bytes[0],
        value_bytes[1],
        value_bytes[2],
        value_bytes[3],
    ];
    let crc = crc5(&payload, 0x1F, true);

    let mut frame = Frame::new();
    push_frame(&mut frame, &HEADER_TX, &payload, crc);
    frame
}

/// Build a read register command (`GetStatus`) — 7 bytes on wire.
#[must_use]
pub fn build_read_register(addr: ChipAddress, reg: u8) -> Frame {
    let cmd = CMD_TYPE | if addr.is_broadcast() { TO_ALL } else { 0 } | CODE_GET_STATUS;
    let len = 5u8; // packed_size=4 + 1 CRC
    let hw_addr = addr.hw_addr();

    let payload = [cmd, len, hw_addr, reg];
    let crc = crc5(&payload, 0x1F, true);

    let mut frame = Frame::new();
    push_frame(&mut frame, &HEADER_TX, &payload, crc);
    frame
}

/// Wire-encode a typed [`ControlCommand`] into a 7-byte frame
/// (`[55 AA] [cmd, 5, hw_addr, 0x00] [CRC5]`). The protocol code,
/// broadcast flag, and hw-addr come from the typed value; this
/// function owns only the framing constants (`CMD_TYPE`, `TO_ALL`)
/// and the CRC.
#[must_use]
pub fn build_command(command: ControlCommand) -> Frame {
    let cmd_byte =
        CMD_TYPE | if command.is_broadcast() { TO_ALL } else { 0 } | command.code().as_u8();
    let payload = [cmd_byte, 5, command.hw_addr(), 0x00];
    let crc = crc5(&payload, 0x1F, true);

    let mut frame = Frame::new();
    push_frame(&mut frame, &HEADER_TX, &payload, crc);
    frame
}

// ---------------------------------------------------------------------------
// Response + parser
// ---------------------------------------------------------------------------

/// A register read-response from the chip.
#[derive(Debug, Clone)]
pub struct RegisterResponse {
    /// Register value returned by the chip.
    pub value: u32,
    /// On-chain address of the responding chip.
    pub chip_addr: u8,
    /// Register address that was read.
    pub reg: u8,
}

/// A parsed chip-to-host response: either a register read-response or a
/// found hardware solution.
#[derive(Debug, Clone)]
pub enum Response {
    /// A register read-back frame.
    RegisterResponse(RegisterResponse),
    /// A found nonce frame.
    HwSolution(HwSolution),
}

/// Parses 11-byte response frames from a byte stream. Caller writes bytes
/// into [`Self::free`] and calls [`Self::commit`] with how many were
/// written. `no_std`-friendly; no global state.
///
/// Desync bytes (bytes discarded while scanning for the `0xAA 0x55` preamble
/// or on CRC5 failure) accumulate in an internal counter. The caller drains
/// it via [`Self::take_desync`] and may forward the count to a global metric.
pub struct ResponseParser {
    buffer: [u8; 256],
    len: usize,
    /// Count of bytes discarded while resyncing to the `0xAA 0x55` preamble
    /// or on CRC5 failure. Drained by [`Self::take_desync`].
    desync_bytes: u32,
}

impl ResponseParser {
    /// Create a new parser with an empty buffer and zero desync count.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [0; 256],
            len: 0,
            desync_bytes: 0,
        }
    }

    /// Return the portion of the buffer not yet filled.
    pub fn free(&mut self) -> &mut [u8] {
        &mut self.buffer[self.len..]
    }

    /// Advance the filled length by `n` bytes written into [`Self::free`].
    pub fn commit(&mut self, n: usize) {
        self.len += n;
    }

    /// Discard all buffered bytes. The desync counter is untouched;
    /// it is drained only by [`Self::take_desync`] so recovery paths
    /// do not lose counts still owed to the metric.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Drain and return the accumulated desync byte count, resetting it to
    /// zero. The caller is responsible for forwarding this count to any
    /// global metric (such as a `portable_atomic::AtomicU32`).
    pub fn take_desync(&mut self) -> u32 {
        let count = self.desync_bytes;
        self.desync_bytes = 0;
        count
    }

    /// Attempt to pop the next complete response frame. Returns `None` when
    /// fewer than `RESPONSE_LEN` bytes are buffered or when the buffer
    /// contains only partial / invalid frames.
    pub fn pop(&mut self, chip: Chip) -> Option<Response> {
        loop {
            while self.len >= 2 && (self.buffer[0] != 0xAA || self.buffer[1] != 0x55) {
                self.drop_first();
            }
            if self.len < RESPONSE_LEN {
                return None;
            }
            let frame = &self.buffer[..RESPONSE_LEN];
            let is_work = frame[10] & 0x80 != 0;
            let init = if is_work {
                CRC5_INIT_WORK_RESPONSE
            } else {
                CRC5_INIT_REG_RESPONSE
            };
            if crc5(&frame[2..10], init, false) != frame[10] & 0x1F {
                self.drop_first();
                continue;
            }
            let response = if is_work {
                let work_id = chip.decode_response_work_id(frame[7]);
                Response::HwSolution(HwSolution {
                    nonce: u32::from_le_bytes([frame[2], frame[3], frame[4], frame[5]]),
                    _extra_diff: frame[6],
                    work_id,
                    version_idx: u16::from_be_bytes([frame[8], frame[9]]),
                })
            } else {
                Response::RegisterResponse(RegisterResponse {
                    value: u32::from_be_bytes([frame[2], frame[3], frame[4], frame[5]]),
                    chip_addr: frame[6],
                    reg: frame[7],
                })
            };
            self.buffer.copy_within(RESPONSE_LEN..self.len, 0);
            self.len -= RESPONSE_LEN;
            return Some(response);
        }
    }

    fn drop_first(&mut self) {
        self.buffer.copy_within(1..self.len, 0);
        self.len -= 1;
        self.desync_bytes += 1;
    }
}

impl Default for ResponseParser {
    fn default() -> Self {
        Self::new()
    }
}
