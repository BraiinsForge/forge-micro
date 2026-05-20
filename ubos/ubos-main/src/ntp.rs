//! SNTP response parser and UTC formatter.

use ubos_core::consts::Ntp;

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
mod tests {
    use super::*;

    #[test]
    fn parse_valid_response() {
        let mut packet = [0u8; 48];
        // NTP seconds for 2025-01-01 00:00:00 UTC = 3_944_764_800.
        let ntp_secs: u32 = 3_944_764_800;
        packet[40..44].copy_from_slice(&ntp_secs.to_be_bytes());
        let unix = parse_timestamp_secs(&packet);
        assert_eq!(unix, Some(u64::from(ntp_secs) - NTP_TO_UNIX_OFFSET));
    }

    #[test]
    fn parse_short_response() {
        assert_eq!(parse_timestamp_secs(&[0u8; 47]), None);
    }

    #[test]
    fn parse_empty_response() {
        assert_eq!(parse_timestamp_secs(&[]), None);
    }

    #[test]
    fn parse_ntp_seconds_before_unix_epoch() {
        // NTP seconds value before Unix epoch should return None.
        let mut packet = [0u8; 48];
        let ntp_secs: u32 = 1_000_000;
        packet[40..44].copy_from_slice(&ntp_secs.to_be_bytes());
        assert_eq!(parse_timestamp_secs(&packet), None);
    }

    #[test]
    fn format_midnight() {
        let mut buffer = [0u8; 16];
        let length = format_utc_time(&mut buffer, 0);
        assert_eq!(&buffer[..length], b"00:00:00 UTC");
    }

    #[test]
    fn format_end_of_day() {
        let mut buffer = [0u8; 16];
        let length = format_utc_time(&mut buffer, 86_399);
        assert_eq!(&buffer[..length], b"23:59:59 UTC");
    }

    #[test]
    fn format_midday() {
        let mut buffer = [0u8; 16];
        // 12:30:45 = 12*3600 + 30*60 + 45 = 45045.
        let length = format_utc_time(&mut buffer, 45_045);
        assert_eq!(&buffer[..length], b"12:30:45 UTC");
    }

    #[test]
    fn format_wraps_past_midnight() {
        let mut buffer = [0u8; 16];
        // One full day + 3661 seconds = 01:01:01.
        let length = format_utc_time(&mut buffer, SECONDS_PER_DAY + 3661);
        assert_eq!(&buffer[..length], b"01:01:01 UTC");
    }
}
