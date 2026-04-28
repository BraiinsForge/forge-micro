//! UART read-back self-test against a Linux serial device. Writes an
//! incrementing byte sequence on `Tx`, reads it back on `Rx`, compares,
//! and logs a heartbeat with rounds/bytes/mismatches/timeouts. Mirrors
//! `firmware/rp2350/src/uart_loopback.rs` so the same exercise covers
//! the host path with whatever device is wired into `/dev/ttyUSB*`
//! (e.g. an external loopback plug, or the rp2350 itself).
//!
//! Counters use `Relaxed` ordering — they are read-mostly and do not
//! synchronize anything else.
//!
//! With nothing electrically looped on the line, reads will time out
//! every round and the `timeouts` counter climbs while the runner
//! keeps the rest of its tasks running.

use std::sync::atomic::{AtomicU64, Ordering};

use embassy_time::{with_timeout, Duration, Timer};
use embedded_io_async::{Read, Write};
use embedded_serial_unix::{Rx, Tx};

const CHUNK_BYTES: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

static ROUNDS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static MISMATCHES: AtomicU64 = AtomicU64::new(0);
static TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// Drive the loopback: write, read with a per-round timeout, compare,
/// repeat. Errors are logged and the loop continues so a transient
/// device hiccup does not stall the runner.
#[embassy_executor::task]
pub async fn loopback_task(mut rx: Rx, mut tx: Tx) -> ! {
    let mut counter: u8 = 0;
    let mut write_buffer = [0u8; CHUNK_BYTES];
    let mut read_buffer = [0u8; CHUNK_BYTES];

    loop {
        for byte in &mut write_buffer {
            *byte = counter;
            counter = counter.wrapping_add(1);
        }

        if let Err(error) = tx.write_all(&write_buffer).await {
            log::warn!("uart loopback: write failed: {error}");
            continue;
        }

        let mut filled: usize = 0;
        let read_outcome = with_timeout(READ_TIMEOUT, async {
            while filled < read_buffer.len() {
                match rx.read(&mut read_buffer[filled..]).await {
                    Ok(0) => break,
                    Ok(count) => filled += count,
                    Err(error) => {
                        log::warn!("uart loopback: read failed: {error}");
                        break;
                    }
                }
            }
        })
        .await;

        if read_outcome.is_err() {
            TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        if filled != write_buffer.len() || read_buffer[..filled] != write_buffer[..] {
            MISMATCHES.fetch_add(1, Ordering::Relaxed);
            // Reset counter alignment so the next round starts from a known state.
            counter = 0;
            continue;
        }

        ROUNDS.fetch_add(1, Ordering::Relaxed);
        // CHUNK_BYTES is 16; the cast to u64 is lossless on every
        // platform with a 16-bit-or-wider usize.
        #[allow(clippy::cast_possible_truncation)]
        BYTES.fetch_add(write_buffer.len() as u64, Ordering::Relaxed);
    }
}

/// Periodic stats line for the loopback. Independent task so the data
/// path is never blocked by logging cadence.
#[embassy_executor::task]
pub async fn heartbeat_task() -> ! {
    loop {
        Timer::after(HEARTBEAT_INTERVAL).await;
        log::info!(
            "uart loopback: rounds={} bytes={} mismatches={} timeouts={}",
            ROUNDS.load(Ordering::Relaxed),
            BYTES.load(Ordering::Relaxed),
            MISMATCHES.load(Ordering::Relaxed),
            TIMEOUTS.load(Ordering::Relaxed),
        );
    }
}
