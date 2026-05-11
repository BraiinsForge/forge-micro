//! Wire-level encoder/decoder for the dummy-asic UART link.
//!
//! Mirrors the protocol previously implemented in
//! `eth_examples::uart_asic`; opcodes, frame lengths, and baud
//! rate must move in lockstep with `boards/eth-examples/dummy-
//! asic/src/protocol.rs` on the peer side or the link desyncs at
//! boot.

use stratum_client::WorkAssignment;

/// Baud rate shared across all board UART configurations for this
/// protocol.
pub const BAUD_RATE: u32 = 921_600;

const FRAME_START: u8 = 0xA5;
const OPCODE_WORK: u8 = 0x01;
const OPCODE_SHARE: u8 = 0x02;
pub(crate) const WORK_FRAME_LEN: usize = 119;
pub(crate) const SHARE_FRAME_LEN: usize = 43;

/// Outbound Work frame. Carries the 80-byte block header (with
/// `nonce = 0` left as the starting point), the 32-byte target,
/// and the 4-byte `work_id` the peer echoes back in every share.
///
/// `header[76..80]` is the nonce field; the peer scans it from
/// `0x0000_0000` upward.
pub(crate) fn encode_work(work: &WorkAssignment, frame: &mut [u8; WORK_FRAME_LEN]) {
    frame[0] = FRAME_START;
    frame[1] = OPCODE_WORK;
    write_header(&mut frame[2..82], work);
    frame[82..114].copy_from_slice(&work.target);
    frame[114..118].copy_from_slice(&work.work_id.to_be_bytes());
    frame[WORK_FRAME_LEN - 1] = xor_checksum(&frame[1..WORK_FRAME_LEN - 1]);
}

/// Materialise the 80-byte block header for `work` with the nonce
/// field set to zero. The peer scans from there.
fn write_header(out: &mut [u8], work: &WorkAssignment) {
    debug_assert_eq!(out.len(), 80);
    out[0..4].copy_from_slice(&work.version.to_le_bytes());
    out[4..36].copy_from_slice(&work.prev_hash);
    out[36..68].copy_from_slice(&work.merkle_root);
    out[68..72].copy_from_slice(&work.ntime.to_le_bytes());
    out[72..76].copy_from_slice(&work.nbits.to_le_bytes());
    out[76..80].fill(0);
}

/// Decoded share payload. The peer ships `(nonce, work_id)` only;
/// `ntime` / `version_bits` are reconstructed from the
/// `WorkAssignment` the host last sent (the peer firmware does not
/// roll either today).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShareFields {
    pub nonce: u32,
    pub work_id: u32,
}

/// Failures the share decoder can surface.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DecodeError {
    /// Opcode byte was not `OPCODE_SHARE`.
    OpcodeMismatch,
    /// XOR-8 checksum failed.
    Checksum,
}

/// Parse a fully-buffered share frame starting at the second byte
/// (the `FRAME_START` byte is consumed by the resynchronisation
/// loop in `run`).
pub(crate) fn decode_share(frame: &[u8; SHARE_FRAME_LEN]) -> Result<ShareFields, DecodeError> {
    if frame[1] != OPCODE_SHARE {
        return Err(DecodeError::OpcodeMismatch);
    }
    let expected = xor_checksum(&frame[1..SHARE_FRAME_LEN - 1]);
    if frame[SHARE_FRAME_LEN - 1] != expected {
        return Err(DecodeError::Checksum);
    }
    let nonce = u32::from_be_bytes([frame[2], frame[3], frame[4], frame[5]]);
    let work_id = u32::from_be_bytes([frame[6], frame[7], frame[8], frame[9]]);
    Ok(ShareFields { nonce, work_id })
}

pub(crate) const fn frame_start() -> u8 {
    FRAME_START
}

fn xor_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &byte| acc ^ byte)
}
