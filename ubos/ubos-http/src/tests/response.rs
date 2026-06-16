//! Tests for `format_content_length` — pure ASCII-decimal formatter.
//!
//! Every HTTP response on every board feeds the result of this function
//! into the `Content-Length` header, so silent truncation or off-by-one
//! errors produce malformed responses on every endpoint.

extern crate std;

use crate::response::format_content_length;

#[test]
fn formats_zero() {
    assert_eq!(format_content_length(0).as_str(), "0");
}

#[test]
fn formats_single_digit() {
    assert_eq!(format_content_length(1).as_str(), "1");
    assert_eq!(format_content_length(9).as_str(), "9");
}

#[test]
fn formats_two_digits() {
    assert_eq!(format_content_length(10).as_str(), "10");
}

#[test]
fn formats_three_digits() {
    assert_eq!(format_content_length(100).as_str(), "100");
}

#[test]
fn formats_five_digits() {
    assert_eq!(format_content_length(12345).as_str(), "12345");
}

#[test]
fn formats_usize_max() {
    let expected = std::format!("{}", usize::MAX);
    assert_eq!(format_content_length(usize::MAX).as_str(), expected);
}
