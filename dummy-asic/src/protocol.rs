//! UART framing for the dummy-ASIC mining protocol.
//!
//! Wire-level frame definitions and the reader / writer helpers used by the
//! UART tasks.  Kept free of shared-state access so the codec can be read in
//! isolation; frame transport is the caller's responsibility.

use embassy_futures::yield_now;
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use esp_hal::uart::{UartRx, UartTx};

// Wire format mirrored on the host side in
// boards/eth-examples/src/dummy_asic.rs; opcodes, frame lengths, baud rate,
// and the temperature sentinel must move in lockstep with that file or the
// link desyncs at boot.

/// UART baud rate for the dummy-ASIC protocol.
// Keep in sync with `eth_examples::uart_asic::BAUD_RATE`; the two crates
// cannot share a dependency without dragging ARM HAL crates into the ESP
// build, so the value is duplicated here.
pub(crate) const BAUD_RATE: u32 = 921_600;

/// Start byte for all frames.
const FRAME_START: u8 = 0xA5;

/// Opcode for a Work frame (host → dummy-asic).
const OPCODE_WORK: u8 = 0x01;

/// Opcode for a Share frame (dummy-asic → host).
const OPCODE_SHARE: u8 = 0x02;

/// Opcode for a `ReadTemp` request (host → dummy-asic).
const OPCODE_READ_TEMP: u8 = 0x03;

/// Opcode for a `TempReport` response (dummy-asic → host).
const OPCODE_TEMP_REPORT: u8 = 0x04;

/// Total size of a Work frame in bytes.
const WORK_FRAME_LEN: usize = 115;

/// Total size of a Share frame in bytes.
const SHARE_FRAME_LEN: usize = 39;

/// Total size of a `ReadTemp` request frame in bytes.
const READ_TEMP_FRAME_LEN: usize = 3;

/// Total size of a `TempReport` response frame in bytes.
const TEMP_REPORT_FRAME_LEN: usize = 7;

/// Sentinel in `LATEST_TEMP_MILLI_CELSIUS` meaning no sampler is running.
/// `-40..=125 °C` is the sensor's rated range, so `i32::MIN` cannot collide
/// with a real reading.  On this board the sampler is deliberately absent;
/// see the "Temperature sampling" section in the crate root.  Future boards
/// with working `esp_hal::tsens` support swap in a real task and the
/// protocol plumbing picks up the readings with no other changes.
pub(crate) const UNKNOWN_TEMP_MILLI_CELSIUS: i32 = i32::MIN;

/// A mining work item received from the host.
#[derive(Clone)]
pub(crate) struct Work {
    /// 80-byte Bitcoin block header.
    pub(crate) header: [u8; 80],
    /// 256-bit mining target, little-endian.  A hash is accepted when the
    /// raw SHA-256d digest, interpreted as a 256-bit little-endian integer,
    /// is less than or equal to this value.
    pub(crate) target: [u8; 32],
}

/// A solution produced by the solver task.
pub(crate) struct Solution {
    /// The nonce that satisfied the target.
    pub(crate) nonce: u32,
    /// The SHA-256d hash of the header with `nonce` installed.
    pub(crate) hash: [u8; 32],
}

/// A parsed request from the host.
pub(crate) enum Request {
    /// A new Work item to mine.
    Work(Work),
    /// A `ReadTemp` poll — the writer should emit the cached sample.
    ReadTemp,
}

/// Outcome of a failed `read_exact`.  Distinguishes a UART error from a
/// zero-length read so the caller can log accordingly.
enum ReadError {
    /// Underlying UART returned an error.
    Io,
    /// Stream returned zero bytes before the requested length was reached.
    Eof,
}

/// Read exactly `buffer.len()` bytes from `rx` into `buffer`.
async fn read_exact(
    rx: &mut UartRx<'static, esp_hal::Async>,
    buffer: &mut [u8],
) -> Result<(), ReadError> {
    let mut position = 0;
    while position < buffer.len() {
        let count = AsyncRead::read(rx, &mut buffer[position..])
            .await
            .map_err(|_| ReadError::Io)?;
        if count == 0 {
            return Err(ReadError::Eof);
        }
        position += count;
    }
    Ok(())
}

/// Compute the XOR-8 checksum over a byte slice.
fn xor_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &byte| acc ^ byte)
}

/// Read and parse one request frame (Work or `ReadTemp`) from `rx`.
///
/// The function re-syncs on a bad start byte, unknown opcode, or bad
/// checksum by logging a warning and trying again from the next byte.
///
/// Resync trade-off: once a byte matches `FRAME_START` we read the rest of
/// whatever frame the opcode selects before validating the checksum.  A
/// spurious `0xA5` inside a corrupted stream can therefore swallow a real
/// frame that begins within that window.  Acceptable for a test harness;
/// revisit if this is ever reused on a lossy link.
pub(crate) async fn read_request_frame(rx: &mut UartRx<'static, esp_hal::Async>) -> Request {
    loop {
        // Spin until a byte looks like a start byte.
        let mut start = [0u8; 1];
        match read_exact(rx, &mut start).await {
            Ok(()) => {}
            Err(ReadError::Io) => {
                defmt::warn!("dummy-asic: UART error reading start byte, resyncing");
                continue;
            }
            Err(ReadError::Eof) => {
                // Defensive branch: the esp-hal async UART honors
                // embedded_io_async::Read semantics and suspends until at
                // least one byte is available, so Ok(0) is not produced
                // under normal operation.  Yield before retrying so a
                // misbehaving UART stuck returning Ok(0) cannot starve
                // the executor.
                yield_now().await;
                continue;
            }
        }
        if start[0] != FRAME_START {
            defmt::warn!("dummy-asic: bad start byte {:#04x}, resyncing", start[0]);
            continue;
        }

        // Peek the opcode to pick a frame size.
        let mut opcode = [0u8; 1];
        if read_exact(rx, &mut opcode).await.is_err() {
            defmt::warn!("dummy-asic: read error on opcode, resyncing");
            continue;
        }

        match opcode[0] {
            OPCODE_WORK => {
                if let Some(work) = read_work_tail(rx).await {
                    return Request::Work(work);
                }
            }
            OPCODE_READ_TEMP => {
                if read_temp_tail(rx).await {
                    return Request::ReadTemp;
                }
            }
            other => {
                defmt::warn!("dummy-asic: unknown opcode {:#04x}, resyncing", other);
            }
        }
    }
}

/// Parse the remainder of a Work frame after the start byte and opcode
/// have already been consumed.  Returns `None` on any read error,
/// checksum mismatch, or invalid target; the caller is expected to
/// resync.
async fn read_work_tail(rx: &mut UartRx<'static, esp_hal::Async>) -> Option<Work> {
    // `frame[0]` holds FRAME_START, `frame[1]` holds the opcode we
    // already read; fill in the tail.
    let mut frame = [0u8; WORK_FRAME_LEN];
    frame[0] = FRAME_START;
    frame[1] = OPCODE_WORK;

    match read_exact(rx, &mut frame[2..]).await {
        Ok(()) => {}
        Err(ReadError::Io) => {
            defmt::warn!("dummy-asic: UART error during work frame, resyncing");
            return None;
        }
        Err(ReadError::Eof) => {
            defmt::warn!("dummy-asic: short read during work frame, resyncing");
            return None;
        }
    }

    let expected = xor_checksum(&frame[1..WORK_FRAME_LEN - 1]);
    if frame[WORK_FRAME_LEN - 1] != expected {
        defmt::warn!(
            "dummy-asic: bad work checksum {:#04x} (expected {:#04x}), resyncing",
            frame[WORK_FRAME_LEN - 1],
            expected
        );
        return None;
    }

    let mut header = [0u8; 80];
    header.copy_from_slice(&frame[2..82]);
    let mut target = [0u8; 32];
    target.copy_from_slice(&frame[82..114]);
    Some(Work { header, target })
}

/// Parse the remainder of a `ReadTemp` frame after the start byte and
/// opcode have been consumed.  Returns `true` on success, `false` if the
/// checksum is wrong or the read fails.
async fn read_temp_tail(rx: &mut UartRx<'static, esp_hal::Async>) -> bool {
    let mut frame = [0u8; READ_TEMP_FRAME_LEN];
    frame[0] = FRAME_START;
    frame[1] = OPCODE_READ_TEMP;

    if read_exact(rx, &mut frame[2..]).await.is_err() {
        defmt::warn!("dummy-asic: read error on read-temp checksum, resyncing");
        return false;
    }

    let expected = xor_checksum(&frame[1..READ_TEMP_FRAME_LEN - 1]);
    if frame[READ_TEMP_FRAME_LEN - 1] != expected {
        defmt::warn!(
            "dummy-asic: bad read-temp checksum {:#04x} (expected {:#04x}), resyncing",
            frame[READ_TEMP_FRAME_LEN - 1],
            expected
        );
        return false;
    }

    true
}

/// Encode and send one Share frame over `tx`.
pub(crate) async fn write_share_frame(
    tx: &mut UartTx<'static, esp_hal::Async>,
    solution: &Solution,
) {
    let mut frame = [0u8; SHARE_FRAME_LEN];
    frame[0] = FRAME_START;
    frame[1] = OPCODE_SHARE;
    frame[2..6].copy_from_slice(&solution.nonce.to_be_bytes());
    frame[6..38].copy_from_slice(&solution.hash);
    // Checksum covers opcode + payload (bytes 1..38).
    frame[SHARE_FRAME_LEN - 1] = xor_checksum(&frame[1..SHARE_FRAME_LEN - 1]);

    if let Err(error) = AsyncWrite::write_all(tx, &frame).await {
        defmt::error!(
            "dummy-asic: share write error: {}",
            defmt::Debug2Format(&error)
        );
    }
}

/// Encode and send one `TempReport` frame over `tx`.
pub(crate) async fn write_temp_frame(tx: &mut UartTx<'static, esp_hal::Async>, milli_celsius: i32) {
    let mut frame = [0u8; TEMP_REPORT_FRAME_LEN];
    frame[0] = FRAME_START;
    frame[1] = OPCODE_TEMP_REPORT;
    frame[2..6].copy_from_slice(&milli_celsius.to_be_bytes());
    // Checksum covers opcode + payload (bytes 1..6).
    frame[TEMP_REPORT_FRAME_LEN - 1] = xor_checksum(&frame[1..TEMP_REPORT_FRAME_LEN - 1]);

    if let Err(error) = AsyncWrite::write_all(tx, &frame).await {
        defmt::error!(
            "dummy-asic: temp write error: {}",
            defmt::Debug2Format(&error)
        );
    }
}
