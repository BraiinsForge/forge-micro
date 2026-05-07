//! Tests for `hex::decode_hex`.

use super::*;

#[test]
fn round_trip_lowercase() {
    let mut out = [0u8; 4];
    assert!(decode_hex(b"deadbeef", &mut out));
    assert_eq!(out, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn round_trip_uppercase() {
    let mut out = [0u8; 4];
    assert!(decode_hex(b"DEADBEEF", &mut out));
    assert_eq!(out, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn rejects_odd_length() {
    let mut out = [0u8; 4];
    assert!(!decode_hex(b"deadbee", &mut out));
}

#[test]
fn rejects_short_hex_for_buffer() {
    let mut out = [0u8; 4];
    assert!(!decode_hex(b"deadbe", &mut out));
}

#[test]
fn rejects_non_hex_byte() {
    let mut out = [0u8; 2];
    assert!(!decode_hex(b"deXXdf", &mut out));
}

#[test]
fn empty_input_empty_output() {
    let mut out = [0u8; 0];
    assert!(decode_hex(b"", &mut out));
}
