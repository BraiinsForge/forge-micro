//! Tests for REST-specific helpers: `validate_board_name`.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

use crate::rest::validate_board_name;

// --- M7: validate_board_name ---

/// Board names with no special characters must pass the check.
#[test]
fn validate_board_name_accepts_safe_names() {
    assert!(validate_board_name("BMM Adapter W5500"));
    assert!(validate_board_name("host-runner"));
    assert!(validate_board_name(""));
    assert!(validate_board_name("abc123 !@#$%^&*()"));
}

/// A board name containing a double-quote must be rejected.
#[test]
fn validate_board_name_rejects_double_quote() {
    assert!(!validate_board_name("bad\"name"));
    assert!(!validate_board_name("\"starts"));
    assert!(!validate_board_name("ends\""));
}

/// A board name containing a backslash must be rejected.
#[test]
fn validate_board_name_rejects_backslash() {
    assert!(!validate_board_name("bad\\name"));
    assert!(!validate_board_name("\\starts"));
    assert!(!validate_board_name("ends\\"));
}
