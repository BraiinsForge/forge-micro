//! UART dummy-asic protocol integration test shared by all board firmware.
//!
//! Each board sends a fixed Work frame to the dummy-asic peer and verifies
//! the Share that comes back by rehashing with the returned nonce, then
//! issues a `ReadTemp` poll and logs the cached die temperature.  The
//! peer's own boot-time self-test uses the same header and target; a real
//! mining pool applies the same rehash check.
//!
//! The sequence is two rounds at boot: one Work → Share with rehash
//! verification, then one `ReadTemp` → `TempReport` with framing
//! verification.  Together they exercise the full protocol end-to-end:
//! framing, checksums, nonce search, SHA-256d digest reconstruction, and
//! the temperature read-out.  Callers that want continuous load should
//! loop externally.

use embassy_time::{with_timeout, Duration};
use sha2::{Digest, Sha256};

// Wire format mirrored on the peer side in
// boards/eth-examples/dummy-asic/src/protocol.rs; opcodes, frame lengths,
// baud rate, and the temperature sentinel must move in lockstep with that
// file or the link desyncs at boot.

/// Baud rate shared across all board UART configurations for this test.
pub const BAUD_RATE: u32 = 115_200;

/// Timeout for reading a response frame.  Target=1 resolves in ~256 nonces
/// on average, well under a second on any dummy-asic we ship.  Keep the budget
/// generous so a momentarily busy peer does not fail the test.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

const FRAME_START: u8 = 0xA5;
const OPCODE_WORK: u8 = 0x01;
const OPCODE_SHARE: u8 = 0x02;
const OPCODE_READ_TEMP: u8 = 0x03;
const OPCODE_TEMP_REPORT: u8 = 0x04;
const WORK_FRAME_LEN: usize = 84;
const SHARE_FRAME_LEN: usize = 39;
const READ_TEMP_FRAME_LEN: usize = 3;
const TEMP_REPORT_FRAME_LEN: usize = 7;

/// Sentinel the dummy-asic emits when no sampler is running on that board.
/// See `dummy-asic`'s "Temperature sampling" module doc.
const UNKNOWN_TEMP_MILLI_CELSIUS: i32 = i32::MIN;

/// Dummy 80-byte Bitcoin block header.  Version `0x20000000` at bytes
/// 0–3 little-endian on the wire, rest zero.  Content is arbitrary;
/// the peer's solver searches the nonce field over whatever header
/// arrives.
const TEST_HEADER: [u8; 80] = {
    let mut header = [0u8; 80];
    header[3] = 0x20;
    header
};

/// Require a single trailing zero byte in the raw SHA-256d digest.
/// Resolves in fewer than 256 trials on average.
pub const TEST_TARGET_LEADING_ZERO_BYTES: u8 = 1;

// Guard the `32 - TEST_TARGET_LEADING_ZERO_BYTES` arithmetic below: a
// SHA-256 digest is 32 bytes, so any target beyond that would underflow
// the slice index. Compile-time failure is cheaper than the runtime trap.
const _: () = assert!(TEST_TARGET_LEADING_ZERO_BYTES as usize <= 32);

fn xor_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &byte| acc ^ byte)
}

fn double_sha256(data: &[u8]) -> [u8; 32] {
    let mut first_hasher = Sha256::new();
    first_hasher.update(data);
    let first = first_hasher.finalize();

    let mut second_hasher = Sha256::new();
    second_hasher.update(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second_hasher.finalize());
    out
}

fn build_work_frame() -> [u8; WORK_FRAME_LEN] {
    let mut frame = [0u8; WORK_FRAME_LEN];
    frame[0] = FRAME_START;
    frame[1] = OPCODE_WORK;
    frame[2..82].copy_from_slice(&TEST_HEADER);
    frame[82] = TEST_TARGET_LEADING_ZERO_BYTES;
    frame[WORK_FRAME_LEN - 1] = xor_checksum(&frame[1..WORK_FRAME_LEN - 1]);
    frame
}

fn build_read_temp_frame() -> [u8; READ_TEMP_FRAME_LEN] {
    let mut frame = [0u8; READ_TEMP_FRAME_LEN];
    frame[0] = FRAME_START;
    frame[1] = OPCODE_READ_TEMP;
    frame[READ_TEMP_FRAME_LEN - 1] = xor_checksum(&frame[1..READ_TEMP_FRAME_LEN - 1]);
    frame
}

/// Read exactly `buffer.len()` bytes, applying [`READ_TIMEOUT`] to each
/// individual `read()` call and logging the specific failure mode.  The
/// budget is per-chunk, not per-response, so a slow steady drip stays
/// within budget as long as no single read stalls past the deadline.
async fn read_response<T>(uart: &mut T, label: &'static str, buffer: &mut [u8]) -> Result<(), ()>
where
    T: embedded_io_async::Read,
{
    let mut position = 0;
    while position < buffer.len() {
        let bytes_read = match with_timeout(READ_TIMEOUT, uart.read(&mut buffer[position..])).await
        {
            Ok(Ok(count)) => count,
            Ok(Err(error)) => {
                defmt::error!(
                    "dummy-asic: {} read failed at byte {}: {}",
                    label,
                    position,
                    defmt::Debug2Format(&error)
                );
                return Err(());
            }
            Err(_) => {
                defmt::error!("dummy-asic: {} timeout after {} bytes", label, position);
                return Err(());
            }
        };
        if bytes_read == 0 {
            defmt::error!("dummy-asic: {} EOF after {} bytes", label, position);
            return Err(());
        }
        position += bytes_read;
    }
    Ok(())
}

async fn run_work_round<T>(uart: &mut T) -> Result<(), ()>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    defmt::info!(
        "dummy-asic: sending work (target={} leading zero bytes)",
        TEST_TARGET_LEADING_ZERO_BYTES
    );

    let work = build_work_frame();
    if let Err(error) = uart.write_all(&work).await {
        defmt::error!(
            "dummy-asic: work write failed: {}",
            defmt::Debug2Format(&error)
        );
        return Err(());
    }

    let mut share = [0u8; SHARE_FRAME_LEN];
    // Scan byte-by-byte for FRAME_START so a single dropped or duplicated
    // byte on the wire resyncs within one round instead of corrupting
    // every subsequent round.  Mirrors the resync the dummy-asic side
    // already runs on its read path.
    loop {
        read_response(uart, "share start", &mut share[..1]).await?;
        if share[0] == FRAME_START {
            break;
        }
        defmt::warn!(
            "dummy-asic: skipping {:#04x} while resyncing on share",
            share[0]
        );
    }
    read_response(uart, "share tail", &mut share[1..]).await?;

    if share[1] != OPCODE_SHARE {
        defmt::error!("dummy-asic: bad share opcode {:#04x}", share[1]);
        return Err(());
    }
    let expected = xor_checksum(&share[1..SHARE_FRAME_LEN - 1]);
    if share[SHARE_FRAME_LEN - 1] != expected {
        defmt::error!(
            "dummy-asic: bad share checksum {:#04x} (expected {:#04x})",
            share[SHARE_FRAME_LEN - 1],
            expected
        );
        return Err(());
    }

    // Nonce is big-endian on the wire; the solver installed it little-endian
    // into the header before hashing, so undo that for the rehash.
    let nonce = u32::from_be_bytes([share[2], share[3], share[4], share[5]]);
    let mut reported_hash = [0u8; 32];
    reported_hash.copy_from_slice(&share[6..38]);

    let mut verify_header = TEST_HEADER;
    verify_header[76..80].copy_from_slice(&nonce.to_le_bytes());
    let computed_hash = double_sha256(&verify_header);

    if computed_hash != reported_hash {
        defmt::error!("dummy-asic: hash mismatch for nonce {:08x}", nonce);
        return Err(());
    }

    let tail_start = 32 - TEST_TARGET_LEADING_ZERO_BYTES as usize;
    if !computed_hash[tail_start..].iter().all(|&byte| byte == 0) {
        defmt::error!(
            "dummy-asic: reported hash does not meet target for nonce {:08x}",
            nonce
        );
        return Err(());
    }

    defmt::info!(
        "dummy-asic: share ok nonce={:08x} hash={=[u8]:02x}",
        nonce,
        computed_hash
    );
    crate::hashrate::record_share();
    Ok(())
}

async fn run_temp_round<T>(uart: &mut T) -> Result<(), ()>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    defmt::info!("dummy-asic: reading die temperature");

    let request = build_read_temp_frame();
    if let Err(error) = uart.write_all(&request).await {
        defmt::error!(
            "dummy-asic: temp write failed: {}",
            defmt::Debug2Format(&error)
        );
        return Err(());
    }

    let mut report = [0u8; TEMP_REPORT_FRAME_LEN];
    // Same resync pattern as the share round so a stray byte does not
    // wedge every subsequent round.
    loop {
        read_response(uart, "temp start", &mut report[..1]).await?;
        if report[0] == FRAME_START {
            break;
        }
        defmt::warn!(
            "dummy-asic: skipping {:#04x} while resyncing on temp",
            report[0]
        );
    }
    read_response(uart, "temp tail", &mut report[1..]).await?;

    if report[1] != OPCODE_TEMP_REPORT {
        defmt::error!("dummy-asic: bad temp opcode {:#04x}", report[1]);
        return Err(());
    }
    let expected = xor_checksum(&report[1..TEMP_REPORT_FRAME_LEN - 1]);
    if report[TEMP_REPORT_FRAME_LEN - 1] != expected {
        defmt::error!(
            "dummy-asic: bad temp checksum {:#04x} (expected {:#04x})",
            report[TEMP_REPORT_FRAME_LEN - 1],
            expected
        );
        return Err(());
    }

    let milli_celsius = i32::from_be_bytes([report[2], report[3], report[4], report[5]]);
    if milli_celsius == UNKNOWN_TEMP_MILLI_CELSIUS {
        // Expected on boards where the dummy-asic crate has no live sampler.
        // Protocol round-trip still passed.
        defmt::info!("dummy-asic: temp reported as unavailable (sentinel)");
    } else {
        defmt::info!("dummy-asic: temp {=i32} m°C", milli_celsius);
    }
    Ok(())
}

/// Run the full boot-time exchange against the dummy-asic peer: one Work →
/// Share round with rehash verification, then one `ReadTemp` →
/// `TempReport` round with framing verification.  Logs the outcome of
/// each round via defmt.
///
/// # Errors
///
/// Returns `Err(())` on any write, read, timeout, framing, checksum, or
/// rehash failure.  The specific failure mode is logged at error level.
/// A `TempReport` carrying the "unavailable" sentinel is *not* an error
/// — the protocol round-trip passed, the peer simply has no live sampler.
pub async fn run_test<T>(uart: &mut T) -> Result<(), ()>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    run_work_round(uart).await?;
    run_temp_round(uart).await
}
