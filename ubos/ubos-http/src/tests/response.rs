//! Tests for `format_content_length` — pure ASCII-decimal formatter.
//!
//! Every HTTP response on every board feeds the result of this function
//! into the `Content-Length` header, so silent truncation or off-by-one
//! errors produce malformed responses on every endpoint.

extern crate std;

use crate::response::format_content_length;

#[test]
fn formats_zero() {
    let mut buffer = [0u8; 20];
    assert_eq!(format_content_length(&mut buffer, 0), "0");
}

#[test]
fn formats_single_digit() {
    let mut buffer = [0u8; 20];
    assert_eq!(format_content_length(&mut buffer, 1), "1");
    let mut buffer = [0u8; 20];
    assert_eq!(format_content_length(&mut buffer, 9), "9");
}

#[test]
fn formats_two_digits() {
    let mut buffer = [0u8; 20];
    assert_eq!(format_content_length(&mut buffer, 10), "10");
}

#[test]
fn formats_three_digits() {
    let mut buffer = [0u8; 20];
    assert_eq!(format_content_length(&mut buffer, 100), "100");
}

#[test]
fn formats_five_digits() {
    let mut buffer = [0u8; 20];
    assert_eq!(format_content_length(&mut buffer, 12345), "12345");
}

#[test]
fn formats_usize_max() {
    let mut buffer = [0u8; 20];
    let expected = std::format!("{}", usize::MAX);
    assert_eq!(format_content_length(&mut buffer, usize::MAX), expected);
}
