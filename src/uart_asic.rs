//! `asic_interface::AsicInterface` over the dummy-asic UART protocol.
//! Used by every board firmware to drive a real dummy-asic peer with
//! stratum-supplied work. The esp32s3 firmware wraps it in a local
//! `#[embassy_executor::task]` because its `embassy-executor` version
//! pin (0.9.1) does not match the worker's (0.10.0); the underlying
//! `run_worker` is a plain async fn so the version mismatch only
//! matters at the task constructor boundary.
//!
//! Wire format mirrored on the peer side in
//! `boards/eth-examples/dummy-asic/src/protocol.rs`; opcodes, frame
//! lengths, and baud rate must move in lockstep with that file or the
//! link desyncs at boot.

use asic_interface::{AsicInterface, RawShare, WorkFrame};
use embedded_io_async::{Read, Write};

/// Baud rate shared across all board UART configurations for this protocol.
pub const BAUD_RATE: u32 = 921_600;

const FRAME_START: u8 = 0xA5;
const OPCODE_WORK: u8 = 0x01;
const OPCODE_SHARE: u8 = 0x02;
const WORK_FRAME_LEN: usize = 119;
const SHARE_FRAME_LEN: usize = 43;

fn xor_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &byte| acc ^ byte)
}

/// Discrete failure modes so the worker's log identifies which side of
/// the wire misbehaved.
#[derive(defmt::Format, Debug)]
pub enum UartAsicError {
    /// Underlying UART returned an error during a read.
    Read,
    /// Underlying UART returned an error during a write.
    Write,
    /// Stream ended before the full frame arrived.
    Eof,
    /// Frame opcode did not match what the host expected.
    OpcodeMismatch,
    /// XOR-8 checksum failed on a received frame.
    Checksum,
}

/// `AsicInterface` over a UART carrying the dummy-asic protocol.
///
/// One round = one Work frame out, one Share frame back. Resyncs on a
/// stray byte by scanning until the next `FRAME_START`, mirroring the
/// dummy-asic firmware's own resync. Each Work carries the host-assigned
/// `work_id` on the wire; the peer echoes it in every share, so
/// `run_worker` can drop shares from a superseded job by comparing
/// `share.work_id` against its current job.
pub struct UartAsic<U> {
    uart: U,
}

impl<U> UartAsic<U> {
    /// Wrap an already-configured UART. The caller owns baud-rate setup;
    /// the constant [`BAUD_RATE`] is the value to use.
    pub const fn new(uart: U) -> Self {
        Self { uart }
    }
}

impl<U> AsicInterface for UartAsic<U>
where
    U: Read + Write,
{
    type Error = UartAsicError;

    async fn send_work(&mut self, work: &WorkFrame) -> Result<(), Self::Error> {
        let mut frame = [0u8; WORK_FRAME_LEN];
        frame[0] = FRAME_START;
        frame[1] = OPCODE_WORK;
        frame[2..82].copy_from_slice(&work.header);
        frame[82..114].copy_from_slice(&work.target);
        frame[114..118].copy_from_slice(&work.work_id.to_be_bytes());
        frame[WORK_FRAME_LEN - 1] = xor_checksum(&frame[1..WORK_FRAME_LEN - 1]);
        self.uart
            .write_all(&frame)
            .await
            .map_err(|_| UartAsicError::Write)?;
        self.uart.flush().await.map_err(|_| UartAsicError::Write)?;
        Ok(())
    }

    async fn recv_share(&mut self) -> Result<RawShare, Self::Error> {
        let mut frame = [0u8; SHARE_FRAME_LEN];
        loop {
            read_exact(&mut self.uart, &mut frame[..1]).await?;
            if frame[0] == FRAME_START {
                break;
            }
        }
        read_exact(&mut self.uart, &mut frame[1..]).await?;

        if frame[1] != OPCODE_SHARE {
            return Err(UartAsicError::OpcodeMismatch);
        }
        let expected = xor_checksum(&frame[1..SHARE_FRAME_LEN - 1]);
        if frame[SHARE_FRAME_LEN - 1] != expected {
            return Err(UartAsicError::Checksum);
        }

        let nonce = u32::from_be_bytes([frame[2], frame[3], frame[4], frame[5]]);
        let work_id = u32::from_be_bytes([frame[6], frame[7], frame[8], frame[9]]);
        Ok(RawShare { nonce, work_id })
    }
}

async fn read_exact<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<(), UartAsicError> {
    let mut position = 0;
    while position < buffer.len() {
        let count = reader
            .read(&mut buffer[position..])
            .await
            .map_err(|_| UartAsicError::Read)?;
        if count == 0 {
            return Err(UartAsicError::Eof);
        }
        position += count;
    }
    Ok(())
}
