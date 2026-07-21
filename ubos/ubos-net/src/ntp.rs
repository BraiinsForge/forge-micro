// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! SNTP response parser and UTC formatter, plus the NTP protocol and
//! sync-timing parameters.

use core::fmt::Write as _;

use embassy_time::Duration;
use heapless::String;

#[cfg(test)]
#[path = "tests/ntp.rs"]
mod tests;

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

/// Format a Unix timestamp as the `HH:MM:SS UTC` wall-clock string.
///
/// The result is always exactly 12 ASCII characters, so it fits a
/// `String<12>` with no spare capacity and the formatting write never
/// fails.
#[must_use]
pub fn format_utc_time(unix_secs: u64) -> String<12> {
    let day_secs = unix_secs % SECONDS_PER_DAY;
    let hours = day_secs / SECONDS_PER_HOUR;
    let minutes = (day_secs % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE;
    let seconds = day_secs % SECONDS_PER_MINUTE;

    let mut text = String::new();
    let _ = write!(text, "{hours:02}:{minutes:02}:{seconds:02} UTC");
    debug_assert_eq!(text.len(), 12, "formatted time must be exactly 12 chars");
    text
}
