//! BM1362 frame and parser primitives.
//!
//! Owns the byte layouts the chip understands on the serial line: command
//! frames the host emits ([`Frame`] + builders), the work-frame format
//! ([`HwWork`]) the chip receives, and the parser that pulls register
//! responses and hardware solutions out of a streaming byte buffer
//! ([`ResponseParser`]).

use heapless::Vec as HVec;
use mining_protocol::bm1362_regs::ControlCommand;
use portable_atomic::{AtomicU32, Ordering};
use sha2::{Digest, Sha256};

/// Lifetime-monotonic count of bytes discarded while scanning for the
/// `0xAA 0x55` frame preamble in the UART byte stream. Survives serial-
/// actor recoveries; useful for detecting sustained desync or noise.
pub static UART_DESYNC_BYTES: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// CRC
// ---------------------------------------------------------------------------

/// CRC5 computation for BM1362 serial protocol.
///
/// - `standard=true, initial=0x1F`: request commands (host -> chip)
/// - `standard=false, initial=0x03`: register responses
/// - `standard=false, initial=0x06`: work responses
fn crc5(msg: &[u8], initial: u8, standard: bool) -> u8 {
    const POLY: u8 = 0x05;
    let mut crc = initial & 0x1F;
    for &orig_byte in msg {
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

const BIP320_VERSION_MASK: u32 = 0x1FFF_E000;
const BIP320_VERSION_SHIFT: u32 = 13;

#[derive(Debug, Clone)]
pub struct HwSolution {
    pub nonce: u32,
    /// Byte 6 of the response frame. The chip's meaning for this byte is
    /// undocumented and unused, so it stays underscore-prefixed to
    /// acknowledge the slot without tripping dead-code lints.
    _extra_diff: u8,
    pub work_id: u8,
    pub version_idx: u16,
}

impl HwSolution {
    /// Compute the rolled block version given a base version.
    fn rolled_version(&self, base_version: u32) -> u32 {
        let base = base_version & !BIP320_VERSION_MASK;
        base | ((u32::from(self.version_idx) << BIP320_VERSION_SHIFT) & BIP320_VERSION_MASK)
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
    pub work_id: u8,
    pub nonce_start: u32,
    pub nbits: u32,
    pub ntime: u32,
    pub merkle_root: [u8; 32],
    pub prev_hash: [u8; 32],
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
    pub fn compute_hash(&self, resp: &HwSolution) -> [u8; 32] {
        let version = resp.rolled_version(self.version);
        let header = self.block_header(resp.nonce, version);
        let first = Sha256::digest(header);
        let second = Sha256::digest(first);
        second.into()
    }

    /// Serialize into the 88-byte on-wire frame.
    #[must_use]
    pub fn serialize(&self) -> [u8; 88] {
        let mut buf = [0u8; 88];
        buf[0] = 0x55;
        buf[1] = 0xAA;
        buf[2] = 0x21; // work command
        buf[3] = 0x36; // length = 54
        buf[4] = self.work_id;
        buf[5] = 0x01; // multiversion = 1

        buf[6..10].copy_from_slice(&self.nonce_start.to_le_bytes());
        buf[10..14].copy_from_slice(&self.nbits.to_le_bytes());
        buf[14..18].copy_from_slice(&self.ntime.to_le_bytes());

        // merkle_root: reverse 32 bytes then swap each 4-byte chunk to LE
        let transformed_merkle = transform_hash(&self.merkle_root);
        buf[18..50].copy_from_slice(&transformed_merkle);

        // prev_hash: same transformation
        let transformed_prev = transform_hash(&self.prev_hash);
        buf[50..82].copy_from_slice(&transformed_prev);

        buf[82..86].copy_from_slice(&self.version.to_le_bytes());

        // CRC16-CCITT over bytes 2..86
        let crc = crc16_ccitt(&buf[2..86]);
        buf[86..88].copy_from_slice(&crc.to_be_bytes());

        buf
    }
}

/// Transform a 32-byte hash for `HwWork` frame:
/// 1. Reverse the entire 32 bytes
/// 2. Swap bytes within each 4-byte chunk (u32 LE conversion per chunk)
fn transform_hash(hash: &[u8; 32]) -> [u8; 32] {
    let mut reversed = *hash;
    reversed.reverse();

    let mut result = [0u8; 32];
    for i in 0..8 {
        let offset = i * 4;
        // Swap bytes within each 4-byte chunk (to LE)
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
/// raw command with ≤10 payload bytes (≤13). The fixed 16-byte cap keeps
/// the buffer on the stack.
#[derive(Debug, Clone)]
pub struct Frame(HVec<u8, 16>);

impl Frame {
    fn new() -> Self {
        Frame(HVec::new())
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

// Command type field: VilCtlCmd = 0x02, shifted to bits [7:5]
const CMD_TYPE: u8 = 0x02 << 5; // = 0x40
const TO_ALL: u8 = 0x10;

// Command codes used by the register builders below. The
// register-less control codes (SetChipAddress, ChainInactive) are
// owned by `mining_protocol::bm1362_regs::ControlCode` and reach the wire
// via `build_command`.
const CODE_SET_CONFIG: u8 = 1;
const CODE_GET_STATUS: u8 = 2;

#[derive(Debug, Clone, Copy)]
pub enum RawChipAddress {
    All,
    Chip(u8),
}

impl RawChipAddress {
    fn hw_addr(self) -> u8 {
        match self {
            RawChipAddress::All => 0x00,
            RawChipAddress::Chip(idx) => idx,
        }
    }

    fn is_broadcast(self) -> bool {
        matches!(self, RawChipAddress::All)
    }
}

// The longest command this module builds is 11 bytes (2 preamble +
// 8 payload + 1 CRC for a write-register frame); Frame is 16 bytes.
// All in-module callers respect that bound.
fn push_frame(frame: &mut Frame, header: &[u8], payload: &[u8], crc: u8) {
    frame
        .0
        .extend_from_slice(header)
        .expect("BUG: Frame capacity");
    frame
        .0
        .extend_from_slice(payload)
        .expect("BUG: Frame capacity");
    frame.0.push(crc).expect("BUG: Frame capacity");
}

/// Build a write register command (`SetConfig`) — 11 bytes on wire.
#[must_use]
pub fn build_write_register(addr: RawChipAddress, reg: u8, value: u32) -> Frame {
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
pub fn build_read_register(addr: RawChipAddress, reg: u8) -> Frame {
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
pub fn build_command(cmd: ControlCommand) -> Frame {
    let cmd_byte = CMD_TYPE | if cmd.is_broadcast() { TO_ALL } else { 0 } | cmd.code().as_u8();
    let payload = [cmd_byte, 5, cmd.hw_addr(), 0x00];
    let crc = crc5(&payload, 0x1F, true);

    let mut frame = Frame::new();
    push_frame(&mut frame, &HEADER_TX, &payload, crc);
    frame
}

/// Build a raw command — `[55 AA] [raw_bytes] [CRC5]`. `raw_bytes` must be
/// at most 10 bytes; anything longer overflows the `Frame` buffer and the
/// `expect` in `push_frame` trips.
#[must_use]
pub fn build_raw_command(raw_bytes: &[u8]) -> Frame {
    let crc = crc5(raw_bytes, 0x1F, true);
    let mut frame = Frame::new();
    push_frame(&mut frame, &HEADER_TX, raw_bytes, crc);
    frame
}

// ---------------------------------------------------------------------------
// Response + parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RegisterResponse {
    pub value: u32,
    pub chip_addr: u8,
    pub reg: u8,
}

#[derive(Debug, Clone)]
pub enum Response {
    RegisterResponse(RegisterResponse),
    HwSolution(HwSolution),
}

/// Parses 11-byte response frames from a byte stream. Caller writes bytes
/// into [`Self::free`] and calls [`Self::commit`] with how many were
/// written. `no_std`-friendly.
pub struct ResponseParser {
    buf: [u8; 256],
    len: usize,
}

impl ResponseParser {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; 256],
            len: 0,
        }
    }

    pub fn free(&mut self) -> &mut [u8] {
        &mut self.buf[self.len..]
    }

    pub fn commit(&mut self, n: usize) {
        self.len += n;
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn pop(&mut self) -> Option<Response> {
        loop {
            while self.len >= 2 && (self.buf[0] != 0xAA || self.buf[1] != 0x55) {
                self.drop_first();
            }
            if self.len < RESPONSE_LEN {
                return None;
            }
            let f = &self.buf[..RESPONSE_LEN];
            let is_work = f[10] & 0x80 != 0;
            let init = if is_work {
                CRC5_INIT_WORK_RESPONSE
            } else {
                CRC5_INIT_REG_RESPONSE
            };
            if crc5(&f[2..10], init, false) != f[10] & 0x1F {
                self.drop_first();
                continue;
            }
            let resp = if is_work {
                Response::HwSolution(HwSolution {
                    nonce: u32::from_le_bytes([f[2], f[3], f[4], f[5]]),
                    _extra_diff: f[6],
                    work_id: f[7],
                    version_idx: u16::from_be_bytes([f[8], f[9]]),
                })
            } else {
                Response::RegisterResponse(RegisterResponse {
                    value: u32::from_be_bytes([f[2], f[3], f[4], f[5]]),
                    chip_addr: f[6],
                    reg: f[7],
                })
            };
            self.buf.copy_within(RESPONSE_LEN..self.len, 0);
            self.len -= RESPONSE_LEN;
            return Some(resp);
        }
    }

    fn drop_first(&mut self) {
        self.buf.copy_within(1..self.len, 0);
        self.len -= 1;
        UART_DESYNC_BYTES.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for ResponseParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tests/bm1362.rs"]
mod tests;
