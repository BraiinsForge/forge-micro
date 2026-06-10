//! SNTP response parser and UTC formatter, plus the NTP protocol and
//! sync-timing parameters.

use embassy_time::Duration;

/// NTP time synchronization parameters.
pub struct Ntp;
impl Ntp {
    pub const HOST: &str = "pool.ntp.org";
    pub const PORT: u16 = 123;
    pub const SYNC_INTERVAL: Duration = Duration::from_secs(60);
    /// Delay between the DHCP lease landing and the first NTP fetch, giving
    /// ARP, the gateway, and routing a moment to settle before the DNS query.
    pub const INITIAL_DELAY: Duration = Duration::from_secs(5);
    /// SNTP packet size: 48 bytes for both request and response.
    pub const PACKET_SIZE: usize = 48;
}

/// NTP epoch (1900-01-01) to Unix epoch (1970-01-01) offset in seconds.
const NTP_TO_UNIX_OFFSET: u64 = 2_208_988_800;

const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;

/// Byte offset of the transmit timestamp seconds field.
const TRANSMIT_TIMESTAMP_OFFSET: usize = 40;

/// Extract a Unix timestamp from a 48-byte SNTP response.
///
/// The transmit timestamp at bytes 40..44 contains seconds since the NTP
/// epoch (1900-01-01) in big-endian format. Returns `None` if the
/// response is too short.
#[must_use]
pub fn parse_timestamp_secs(response: &[u8]) -> Option<u64> {
    if response.len() < Ntp::PACKET_SIZE {
        return None;
    }

    let ntp_secs = u32::from_be_bytes([
        response[TRANSMIT_TIMESTAMP_OFFSET],
        response[TRANSMIT_TIMESTAMP_OFFSET + 1],
        response[TRANSMIT_TIMESTAMP_OFFSET + 2],
        response[TRANSMIT_TIMESTAMP_OFFSET + 3],
    ]);

    let unix_secs = u64::from(ntp_secs).checked_sub(NTP_TO_UNIX_OFFSET)?;
    Some(unix_secs)
}

/// Format a Unix timestamp as `HH:MM:SS UTC` into the provided buffer.
///
/// Returns the number of bytes written. The buffer must be at least 12
/// bytes to hold the full formatted string; a shorter buffer trips the
/// debug assertion in debug builds and panics on the slice write in
/// release. Callers must size at compile time.
pub fn format_utc_time(buffer: &mut [u8], unix_secs: u64) -> usize {
    // "HH:MM:SS UTC" = 12 bytes. Callers must size the buffer for the
    // full string; the assertion catches regressions in debug, and the
    // slice write below panics in release rather than silently writing
    // a partial string.
    debug_assert!(buffer.len() >= 12, "format_utc_time needs >=12 bytes");

    let day_secs = unix_secs % SECONDS_PER_DAY;
    let hours = day_secs / SECONDS_PER_HOUR;
    let minutes = (day_secs % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE;
    let seconds = day_secs % SECONDS_PER_MINUTE;

    let output = b" UTC";
    let needed = 8 + output.len();

    // hours < 24, minutes < 60, seconds < 60, so every digit fits in u8.
    buffer[0] = b'0' + (hours / 10) as u8;
    buffer[1] = b'0' + (hours % 10) as u8;
    buffer[2] = b':';
    buffer[3] = b'0' + (minutes / 10) as u8;
    buffer[4] = b'0' + (minutes % 10) as u8;
    buffer[5] = b':';
    buffer[6] = b'0' + (seconds / 10) as u8;
    buffer[7] = b'0' + (seconds % 10) as u8;
    buffer[8..needed].copy_from_slice(output);

    needed
}

#[cfg(test)]
#[path = "tests/ntp.rs"]
mod tests;
