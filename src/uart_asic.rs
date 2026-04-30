//! `asic_interface::AsicInterface` over the dummy-asic UART protocol.
//! Used by ARM firmware to drive a real dummy-asic peer with stratum-
//! supplied work; ESP32-S3 firmware keeps the existing boot-time self-
//! test in `dummy_asic.rs` because its embassy-executor pin precludes
//! pulling in the worker.

use asic_interface::{AsicInterface, RawShare, WorkFrame};
use embedded_io_async::{Read, Write};

use crate::dummy_asic::{
    xor_checksum, FRAME_START, OPCODE_SHARE, OPCODE_WORK, SHARE_FRAME_LEN, WORK_FRAME_LEN,
};

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
/// dummy-asic firmware's own resync. The wire format carries no work
/// identifier, so `recv_share` stamps shares with the `work_id` of the
/// last `send_work`; the dummy-asic firmware abandons the previous Work
/// the moment a new one arrives, leaving only a narrow on-wire window
/// where a stale share can still be packaged as current.
pub struct UartAsic<U> {
    uart: U,
    last_work_id: u32,
}

impl<U> UartAsic<U> {
    /// Wrap an already-configured UART. The caller owns baud-rate setup;
    /// the constant `crate::dummy_asic::BAUD_RATE` is the value to use.
    pub const fn new(uart: U) -> Self {
        Self {
            uart,
            last_work_id: 0,
        }
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
        frame[WORK_FRAME_LEN - 1] = xor_checksum(&frame[1..WORK_FRAME_LEN - 1]);
        self.uart
            .write_all(&frame)
            .await
            .map_err(|_| UartAsicError::Write)?;
        self.uart.flush().await.map_err(|_| UartAsicError::Write)?;
        self.last_work_id = work.work_id;
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
        Ok(RawShare {
            nonce,
            work_id: self.last_work_id,
        })
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
