//! BM1362 frame and parser primitives.
//!
//! Owns the byte layouts the chip understands on the serial line: command
//! frames the host emits ([`Frame`] + builders), the work-frame format
//! ([`HwWork`]) the chip receives, and the parser that pulls register
//! responses and hardware solutions out of a streaming byte buffer
//! ([`ResponseParser`]).

use heapless::Vec as HVec;
use sha2::{Digest, Sha256};

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
/// applies this swap inline; mmm stores `HwWork.prev_hash` in canonical
/// post-swap form so both the local hash check and the chip wire frame
/// see the same bytes.
#[must_use]
pub fn word_swap_prev_hash(input: &[u8; 32]) -> [u8; 32] {
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
/// `prev_hash` is in canonical Bitcoin internal byte order (post the
/// stratum 4-byte word swap); callers reading from a `WorkAssignment`
/// must run the value through [`word_swap_prev_hash`] first.
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

// Command codes
const _CODE_SET_CHIP_ADDRESS: u8 = 0;
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
    dropped_byte: u32,
}

impl ResponseParser {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; 256],
            len: 0,
            dropped_byte: 0,
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
        self.dropped_byte += 1;
    }
}

impl Default for ResponseParser {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc5_broadcast_write_0x58() {
        let payload = [0x51, 0x09, 0x00, 0x58, 0x00, 0x01, 0x11, 0x11];
        assert_eq!(crc5(&payload, 0x1F, true), 0x0D);
    }

    #[test]
    fn crc5_unicast_write_0x58() {
        let payload = [0x41, 0x09, 0x06, 0x58, 0x00, 0x01, 0x11, 0x11];
        assert_eq!(crc5(&payload, 0x1F, true), 0x01);
    }

    #[test]
    fn crc5_broadcast_write_0x54() {
        let payload = [0x51, 0x09, 0x00, 0x54, 0x00, 0x00, 0x00, 0x03];
        assert_eq!(crc5(&payload, 0x1F, true), 0x1D);
    }

    #[test]
    fn crc5_register_response() {
        let data = [0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(crc5(&data, 0x03, false), 0x1E);
    }

    #[test]
    fn work_response_rolled_version() {
        let resp = HwSolution {
            nonce: 0x1ABA_825A,
            _extra_diff: 0,
            work_id: 0xB8,
            version_idx: 312,
        };
        assert_eq!(resp.rolled_version(0x2000_0000), 0x2027_0000);
    }

    #[test]
    fn serialize_work_frame_crc() {
        // From the worked example in BM1362_WORK.md
        let expected_bytes: [u8; 88] = [
            0x55, 0xAA, 0x21, 0x36, 0xB8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x7E, 0x3C, 0x02, 0x17,
            0x91, 0x22, 0x93, 0x69, 0xEE, 0xF3, 0xAC, 0x4B, 0x19, 0x9E, 0x64, 0xA0, 0x40, 0x43,
            0xFD, 0xD8, 0x16, 0xBD, 0x00, 0x37, 0x8D, 0xC6, 0xF6, 0x78, 0xD2, 0xFF, 0xCD, 0xBD,
            0xEA, 0x83, 0x07, 0xE2, 0xC8, 0xCA, 0xB9, 0xC2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x97, 0x30, 0x01, 0x00, 0xC1, 0x8F, 0x5A, 0x80, 0xB5, 0x97, 0xC1, 0x5D,
            0x14, 0x03, 0xC7, 0xEE, 0xD8, 0x81, 0xDC, 0xBB, 0x65, 0x82, 0x9C, 0x62, 0x00, 0x00,
            0x00, 0x20, 0x37, 0xBD,
        ];
        let crc = crc16_ccitt(&expected_bytes[2..86]);
        assert_eq!(crc, 0x37BD);
    }

    #[test]
    fn build_broadcast_write() {
        let frame = build_write_register(RawChipAddress::All, 0x58, 0x0001_1111);
        assert_eq!(
            frame.as_slice(),
            [0x55, 0xAA, 0x51, 0x09, 0x00, 0x58, 0x00, 0x01, 0x11, 0x11, 0x0D]
        );
    }

    #[test]
    fn build_unicast_write() {
        let frame = build_write_register(RawChipAddress::Chip(3), 0x58, 0x0001_1111);
        assert_eq!(
            frame.as_slice(),
            [0x55, 0xAA, 0x41, 0x09, 0x03, 0x58, 0x00, 0x01, 0x11, 0x11, 0x07]
        );
    }

    fn feed(parser: &mut ResponseParser, data: &[u8]) {
        parser.free()[..data.len()].copy_from_slice(data);
        parser.commit(data.len());
    }

    #[test]
    fn parse_register_response() {
        let mut parser = ResponseParser::new();
        feed(
            &mut parser,
            &[
                0xAA, 0x55, 0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E,
            ],
        );
        let resp = parser.pop().expect("should parse");
        match resp {
            Response::RegisterResponse(r) => {
                assert_eq!(r.value, 0x1362_0300);
                assert_eq!(r.chip_addr, 0x00);
                assert_eq!(r.reg, 0x00);
            }
            Response::HwSolution(_) => panic!("expected register response"),
        }
        assert_eq!(parser.dropped_byte, 0);
    }

    #[test]
    fn parse_with_junk() {
        let mut parser = ResponseParser::new();
        feed(
            &mut parser,
            &[
                0xFF, 0xFF, 0xAA, 0x55, 0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E,
            ],
        );
        let resp = parser.pop().expect("should parse despite junk");
        assert!(matches!(resp, Response::RegisterResponse(_)));
        assert_eq!(parser.dropped_byte, 2);
    }

    /// Locks mmm's local share-verification hash to
    /// `mining_protocol::hashing::header_hash`. Both sides reconstruct the
    /// same canonical block header from the same `WorkAssignment` fields,
    /// and the pool will reject any submit whose chip-side hash diverges
    /// from what it recomputes for the same `(prev_hash, ntime, nonce, ...)`.
    #[test]
    fn compute_hash_matches_mining_protocol_reference() {
        let mut stratum_prev = [0u8; 32];
        for (index, byte) in stratum_prev.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let mut merkle = [0u8; 32];
        for (index, byte) in merkle.iter_mut().enumerate() {
            *byte = (32 - index) as u8;
        }
        let version: u32 = 0x2000_0000;
        let ntime: u32 = 0x6700_0000;
        let nbits: u32 = 0x1d00_ffff;
        let nonce: u32 = 0xdead_beef;

        let hw_work = HwWork {
            work_id: 0,
            nonce_start: 0,
            nbits,
            ntime,
            merkle_root: merkle,
            // mmm stores `prev_hash` already word-swapped from stratum
            // wire form so the local hash check produces the same bytes
            // the chip and pool both compute against.
            prev_hash: word_swap_prev_hash(&stratum_prev),
            version,
        };
        let resp = HwSolution {
            nonce,
            _extra_diff: 0,
            work_id: 0,
            version_idx: 0,
        };

        let mmm_hash = hw_work.compute_hash(&resp);
        let reference = mining_protocol::hashing::header_hash(
            version,
            stratum_prev,
            merkle,
            ntime,
            nbits,
            nonce,
        );
        assert_eq!(mmm_hash, reference);
    }
}
