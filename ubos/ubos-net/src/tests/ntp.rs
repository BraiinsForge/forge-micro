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
    assert_eq!(format_utc_time(0).as_str(), "00:00:00 UTC");
}

#[test]
fn format_end_of_day() {
    assert_eq!(format_utc_time(86_399).as_str(), "23:59:59 UTC");
}

#[test]
fn format_midday() {
    // 12:30:45 = 12*3600 + 30*60 + 45 = 45045.
    assert_eq!(format_utc_time(45_045).as_str(), "12:30:45 UTC");
}

#[test]
fn format_wraps_past_midnight() {
    // One full day + 3661 seconds = 01:01:01.
    assert_eq!(
        format_utc_time(SECONDS_PER_DAY + 3661).as_str(),
        "01:01:01 UTC"
    );
}
