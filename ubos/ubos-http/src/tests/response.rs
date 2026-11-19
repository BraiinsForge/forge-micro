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

//! Tests for `format_content_length` — pure ASCII-decimal formatter —
//! and for `message_body`, the escaped body of a message response.
//!
//! Every HTTP response on every board feeds the result of the formatter
//! into the `Content-Length` header, so silent truncation or off-by-one
//! errors produce malformed responses on every endpoint.

extern crate std;

use crate::response::{format_content_length, message_body};

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

/// The body that `write_message` writes for `message`, joined from its
/// pieces.
fn body(message: &str) -> std::vec::Vec<u8> {
    message_body(message).flatten().copied().collect()
}

#[test]
fn message_body_wraps_the_text() {
    assert_eq!(body("busy, retry"), br#"{"message":"busy, retry"}"#);
}

#[test]
fn message_body_escapes_quotes_and_backslashes() {
    assert_eq!(body(r#"a "b" c\d"#), br#"{"message":"a \"b\" c\\d"}"#);
}

#[test]
fn message_body_writes_control_bytes_as_spaces() {
    assert_eq!(body("a\nb\u{1f}"), br#"{"message":"a b "}"#);
}

#[test]
fn message_body_escapes_a_special_byte_at_either_end() {
    assert_eq!(body(r#""x\"#), br#"{"message":"\"x\\"}"#);
}
