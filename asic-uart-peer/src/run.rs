//! `run_asic_uart` — the UART-attached peer end of an
//! [`AsicSession`].
//!
//! Loop shape:
//!
//! 1. Block until the first [`WorkAssignment`] arrives on the
//!    session's `Watch`.
//! 2. Encode it as a Work frame and write to UART.
//! 3. Concurrently await either a Share frame from UART or a
//!    fresh assignment. On a fresh assignment, abandon the
//!    pending share read and loop back to step 2 with the new
//!    work; on a Share frame, build a [`Solution`] (with `ntime`
//!    and `version_bits` filled from the still-current
//!    assignment, since the peer does not roll them) and push it
//!    to the session.
//!
//! Share decoding errors (opcode mismatch, checksum) drop the
//! frame and resync to the next `FRAME_START`, matching the
//! legacy `ubos_main::uart_asic` behaviour.

use embassy_futures::select::{select, Either};
use embedded_io_async::{Read, Write};
use stratum_client::{AsicSession, Solution};
use ubos_logging::{info, trace, warn};

use crate::wire::{
    decode_share, encode_work, frame_start, DecodeError, SHARE_FRAME_LEN, WORK_FRAME_LEN,
};

/// Drive the UART peer forever.
///
/// `uart` is the configured Read+Write handle to the peer; baud
/// rate must be set to [`crate::BAUD_RATE`] (`921_600`).
///
/// # Panics
///
/// Panics if `asic.work_receiver()` has no free slot. With the
/// default `AsicSession::new()` and a single peer that won't
/// happen; misuse from the firmware (e.g. multiple consumers on
/// the same `AsicSession`) is the only way to trigger it.
pub async fn run_asic_uart<U: Read + Write>(asic: &'static AsicSession, mut uart: U) -> ! {
    let mut receiver = asic
        .work_receiver()
        .expect("BUG: AsicSession Watch must have a free receiver slot");

    // Block until the host publishes the first WorkAssignment.
    let mut current = receiver.changed().await;
    info!(
        "asic-uart-peer: first work received, work_id={}",
        current.work_id,
    );

    loop {
        // Encode + send the current assignment to the peer.
        let mut frame = [0u8; WORK_FRAME_LEN];
        encode_work(&current, &mut frame);
        if write_all(&mut uart, &frame).await.is_err() {
            warn!("asic-uart-peer: send_work failed");
            current = receiver.changed().await;
            continue;
        }

        // Race a share read against a new assignment. Either path
        // re-enters the top of the loop with the most recent work.
        match select(read_share(&mut uart), receiver.changed()).await {
            Either::First(Ok(share)) => {
                let solution = Solution {
                    work_id: share.work_id,
                    nonce: share.nonce,
                    // Peer does not roll either; mirror back what
                    // we last sent so the host pairs the share
                    // with the correct OutstandingWork slot.
                    ntime: current.ntime,
                    version_bits: 0,
                };
                info!(
                    "asic-uart-peer: solution received work_id={} nonce={:#010x}",
                    solution.work_id, solution.nonce,
                );
                ubos_main::hashrate::record_share(&current.target);
                let queued = asic.submit_solution(solution);
                if !queued {
                    warn!("asic-uart-peer: solutions channel full, dropped");
                }

                // Check the watch in case fresh work landed while
                // we were processing this share; otherwise stay
                // on the current assignment and re-send it so the
                // peer's nonce counter restarts.
                if let Some(fresh) = receiver.try_changed() {
                    current = fresh;
                }
            }
            Either::First(Err(error)) => {
                warn!("asic-uart-peer: recv_share failed: {:?}", error);
                // Wait for a fresh assignment to retry; a torn UART
                // mid-frame is best paired with a fresh work send
                // anyway.
                current = receiver.changed().await;
            }
            Either::Second(fresh) => {
                trace!("asic-uart-peer: work refreshed work_id={}", fresh.work_id,);
                current = fresh;
            }
        }
    }
}

/// Failures the share reader can surface.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
enum ReadShareError {
    /// Underlying UART returned an error.
    Io,
    /// Stream ended before a full frame arrived.
    Eof,
    /// Decode rejected the frame (bad opcode or checksum).
    Decode,
}

/// Resync on `FRAME_START`, then read the rest of the share frame
/// and decode.
async fn read_share<U: Read>(uart: &mut U) -> Result<crate::wire::ShareFields, ReadShareError> {
    let mut frame = [0u8; SHARE_FRAME_LEN];
    loop {
        read_exact(uart, &mut frame[..1]).await?;
        if frame[0] == frame_start() {
            break;
        }
    }
    read_exact(uart, &mut frame[1..]).await?;
    decode_share(&frame).map_err(|err| match err {
        DecodeError::OpcodeMismatch | DecodeError::Checksum => ReadShareError::Decode,
    })
}

async fn read_exact<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<(), ReadShareError> {
    let mut position = 0;
    while position < buffer.len() {
        let count = reader
            .read(&mut buffer[position..])
            .await
            .map_err(|_| ReadShareError::Io)?;
        if count == 0 {
            return Err(ReadShareError::Eof);
        }
        position += count;
    }
    Ok(())
}

async fn write_all<W: Write>(writer: &mut W, data: &[u8]) -> Result<(), ()> {
    writer.write_all(data).await.map_err(|_| ())?;
    writer.flush().await.map_err(|_| ())?;
    Ok(())
}
