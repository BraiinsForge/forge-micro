//! UART echo test protocol shared by all board firmware.
//!
//! Each board sends a test line to the echoer and verifies the reply.
//! The echoer is a dedicated XIAO ESP32-S3 running an async echo
//! loop at 115200 baud. This module provides the protocol constants
//! and a generic async test function that works over any
//! `embedded_io_async::Read + Write` transport.

use embassy_time::{with_timeout, Duration};

/// Per-read timeout. Generous for 115200 baud where 12 bytes take under 1 ms.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Baud rate shared between echoer and all board UART configurations.
/// The echoer crate imports this constant directly.
pub const BAUD_RATE: u32 = 115_200;

/// Test payload sent to the echoer and expected back verbatim.
const TEST_LINE: &[u8] = b"hello world\n";

/// Maximum bytes to read back before giving up.
const MAX_REPLY_LEN: usize = 256;

/// Run a single echo round-trip over the given UART transport.
///
/// Sends `TEST_LINE`, reads back up to the trailing `\n`, and compares.
/// Logs success or failure via defmt. Returns `Ok(())` on match,
/// `Err(())` on mismatch or I/O error.
///
/// # Errors
///
/// Returns `Err(())` if the write fails, the read fails, the read
/// returns zero bytes, or the echoed reply does not match the sent line.
pub async fn run_test<T>(uart: &mut T) -> Result<(), ()>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    defmt::info!("uart echo: sending test line");

    if let Err(error) = uart.write_all(TEST_LINE).await {
        defmt::error!("uart echo: write failed: {}", defmt::Debug2Format(&error));
        return Err(());
    }

    let mut buffer = [0u8; MAX_REPLY_LEN];
    let mut position = 0;

    while position < MAX_REPLY_LEN {
        let bytes_read = match with_timeout(READ_TIMEOUT, uart.read(&mut buffer[position..])).await
        {
            Ok(Ok(count)) => count,
            Ok(Err(error)) => {
                defmt::error!("uart echo: read failed: {}", defmt::Debug2Format(&error));
                return Err(());
            }
            Err(_) => {
                defmt::error!("uart echo: read timed out");
                return Err(());
            }
        };
        if bytes_read == 0 {
            defmt::error!("uart echo: read returned zero bytes");
            return Err(());
        }
        position += bytes_read;

        // Check whether we have received the newline terminator yet.
        if buffer[..position].contains(&b'\n') {
            break;
        }
    }

    let reply = &buffer[..position];
    if reply == TEST_LINE {
        defmt::info!("uart echo: success");
        Ok(())
    } else {
        defmt::error!(
            "uart echo: mismatch, expected {} bytes, got {}",
            TEST_LINE.len(),
            position
        );
        Err(())
    }
}
