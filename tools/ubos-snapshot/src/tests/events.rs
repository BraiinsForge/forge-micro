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

//! Tests for width parsing, argument decoding, message formatting, and
//! timestamp formatting.

use crate::events::{EventCodec, Width};

#[test]
fn width_parses_all_six_variants() {
    assert_eq!(Width::parse("u8").expect("u8 is a valid width"), Width::U8);
    assert_eq!(
        Width::parse("u16").expect("u16 is a valid width"),
        Width::U16
    );
    assert_eq!(
        Width::parse("u32").expect("u32 is a valid width"),
        Width::U32
    );
    assert_eq!(Width::parse("i8").expect("i8 is a valid width"), Width::I8);
    assert_eq!(
        Width::parse("i16").expect("i16 is a valid width"),
        Width::I16
    );
    assert_eq!(
        Width::parse("i32").expect("i32 is a valid width"),
        Width::I32
    );
}

#[test]
fn width_rejects_unknown_string() {
    assert!(Width::parse("u64").is_err());
    assert!(Width::parse("").is_err());
}

#[test]
fn decode_args_reads_le_and_sign_extends_negative() {
    // u16 9500 (0x251c) then i8 -5 (0xfb), zero-padded to 11 bytes.
    let widths = [Width::U16, Width::I8];
    let values = EventCodec::decode_args("1c25fb0000000000000000", &widths)
        .expect("well-formed fixed-width payload");
    assert_eq!(values, vec![9500, -5]);
}

#[test]
fn decode_args_sign_extends_positive_and_wide_values() {
    // i16 300 (0x012c), u32 0xdeadbeef.
    let widths = [Width::I16, Width::U32];
    let values = EventCodec::decode_args("2c01efbeadde0000000000", &widths)
        .expect("well-formed fixed-width payload");
    assert_eq!(values, vec![300, 0xdead_beef_i64]);
}

#[test]
fn decode_args_rejects_short_payload() {
    let widths = [Width::U32, Width::U32, Width::U32];
    assert!(EventCodec::decode_args("00000000", &widths).is_err());
}

#[test]
fn decode_args_rejects_odd_length_hex() {
    assert!(EventCodec::decode_args("abc", &[Width::U8]).is_err());
}

#[test]
fn decode_args_rejects_non_hex_digit_in_even_length_payload() {
    // "zz" is even-length, so it must fail in the hex-digit parse
    // rather than the odd-length check, and it must error rather than
    // panic.
    assert!(EventCodec::decode_args("zz", &[Width::U8]).is_err());
}

#[test]
fn format_message_substitutes_in_order() {
    let out = EventCodec::format_message("a={} b={}", &[1, -2]);
    assert_eq!(out, "a=1 b=-2");
}

#[test]
fn format_message_renders_missing_placeholders_as_empty() {
    let out = EventCodec::format_message("a={} b={} c={}", &[1]);
    assert_eq!(out, "a=1 b= c=");
}

#[test]
fn format_time_pre_sync_shows_elapsed_seconds() {
    assert_eq!(EventCodec::format_time(42), "42s (pre-sync)");
    assert_eq!(
        EventCodec::format_time(999_999_999),
        "999999999s (pre-sync)"
    );
}

#[test]
fn format_time_negative_timestamp_renders_without_panicking() {
    // Negative values fall below the pre-sync/post-sync threshold, so
    // they take the pre-sync branch and render as elapsed seconds, even
    // though a real device never records negative elapsed time.
    assert_eq!(EventCodec::format_time(-5), "-5s (pre-sync)");
}

#[test]
fn format_time_post_sync_shows_utc_calendar_time() {
    assert_eq!(
        EventCodec::format_time(1_721_000_000),
        "2024-07-14 23:33:20 UTC"
    );
    assert_eq!(
        EventCodec::format_time(1_000_000_000),
        "2001-09-09 01:46:40 UTC"
    );
}
