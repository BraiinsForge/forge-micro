//! Tests for `auth::authenticate` and the base64 decoder.

extern crate std;

use crate::auth::{
    authenticate, constant_time_eq, extract_session_cookie_from_str, AuthCtx, AuthError,
};

// --- basic auth via Authorization header ---

#[test]
fn basic_auth_root_root_accepted() {
    // The credential store is now eagerly populated by `init_credentials`
    // at the top of each listener task at boot; tests have no boot path,
    // so each test that depends on the defaults invokes the helper
    // directly. It is idempotent, so cross-test ordering does not matter.
    embassy_futures::block_on(crate::auth::init_credentials());
    // "root:root" in base64 is "cm9vdDpyb290"
    let result = embassy_futures::block_on(authenticate(Some("Basic cm9vdDpyb290"), None));
    assert!(
        matches!(result, Ok(AuthCtx::Basic)),
        "root:root should authenticate as Basic"
    );
}

#[test]
fn basic_auth_wrong_password_rejected() {
    // "root:wrong" in base64 is "cm9vdDp3cm9uZw=="
    let result = embassy_futures::block_on(authenticate(Some("Basic cm9vdDp3cm9uZw=="), None));
    assert!(
        matches!(result, Err(AuthError::BadCredentials)),
        "wrong password should return BadCredentials"
    );
}

#[test]
fn no_authorization_header_returns_anonymous() {
    let result = embassy_futures::block_on(authenticate(None, None));
    assert!(
        matches!(result, Ok(AuthCtx::Anonymous)),
        "missing header should return Anonymous"
    );
}

#[test]
fn malformed_basic_header_rejected() {
    // Not valid base64.
    let result = embassy_futures::block_on(authenticate(Some("Basic !!!notbase64!!!"), None));
    assert!(
        matches!(result, Err(AuthError::BadFormat)),
        "invalid base64 should return BadFormat"
    );
}

#[test]
fn basic_header_missing_colon_rejected() {
    // "rootroot" has no colon separator; base64 of "rootroot" is "cm9vdHJvb3Q="
    let result = embassy_futures::block_on(authenticate(Some("Basic cm9vdHJvb3Q="), None));
    assert!(
        matches!(result, Err(AuthError::BadFormat)),
        "decoded value without colon should return BadFormat"
    );
}

// --- base64 edge cases reachable through `authenticate_basic` ---

#[test]
fn basic_header_equals_in_middle_rejected() {
    // `cm9v=Dpyb290` puts `=` in a non-terminal position. The decoder
    // must reject this rather than silently treating `=` as zero.
    let result = embassy_futures::block_on(authenticate(Some("Basic cm9v=Dpyb290"), None));
    assert!(
        matches!(result, Err(AuthError::BadFormat)),
        "`=` mid-string must be rejected as malformed base64"
    );
}

#[test]
fn basic_header_three_pads_rejected() {
    // `Y===` is len-4 with three pads, which RFC 4648 disallows.
    let result = embassy_futures::block_on(authenticate(Some("Basic Y==="), None));
    assert!(
        matches!(result, Err(AuthError::BadFormat)),
        "three-byte trailing padding must be rejected"
    );
}

#[test]
fn basic_header_url_safe_alphabet_rejected() {
    // `-` and `_` are URL-safe base64. Standard HTTP Basic auth uses
    // the standard alphabet (`+`, `/`); URL-safe should not decode.
    let result = embassy_futures::block_on(authenticate(Some("Basic cm9-vDpyb290"), None));
    assert!(
        matches!(result, Err(AuthError::BadFormat)),
        "URL-safe alphabet must be rejected"
    );
}

#[test]
fn basic_header_non_multiple_of_four_rejected() {
    // `cm9vdDpyb290abc` is 15 chars (not a multiple of 4 after trim).
    let result = embassy_futures::block_on(authenticate(Some("Basic cm9vdDpyb290abc"), None));
    assert!(
        matches!(result, Err(AuthError::BadFormat)),
        "base64 with non-multiple-of-4 length must be rejected"
    );
}

// --- extract_session_cookie_from_str edges ---

#[test]
fn session_cookie_extracted_when_alone() {
    assert_eq!(
        extract_session_cookie_from_str(Some("session=abc123def")),
        Some("abc123def")
    );
}

#[test]
fn session_cookie_extracted_with_other_cookies() {
    assert_eq!(
        extract_session_cookie_from_str(Some("pref=dark; session=xyz789; tracker=on")),
        Some("xyz789")
    );
}

#[test]
fn missing_cookie_header_returns_none() {
    assert_eq!(extract_session_cookie_from_str(None), None);
}

#[test]
fn cookie_header_without_session_returns_none() {
    assert_eq!(
        extract_session_cookie_from_str(Some("pref=dark; tracker=on")),
        None
    );
}

#[test]
fn cookie_with_empty_session_value_returns_empty_string() {
    assert_eq!(extract_session_cookie_from_str(Some("session=")), Some(""));
}

// --- constant_time_eq length-mismatch and final-byte mismatch ---

#[test]
fn constant_time_eq_rejects_length_mismatch() {
    assert!(!constant_time_eq("abc", "abcd"));
    assert!(!constant_time_eq("", "x"));
    assert!(!constant_time_eq(
        "longer-string-here-yes",
        "longer-string-here-no"
    ));
}

#[test]
fn constant_time_eq_rejects_final_byte_mismatch() {
    assert!(!constant_time_eq("rootrootroot1", "rootrootroot2"));
    assert!(!constant_time_eq("a", "b"));
}

#[test]
fn constant_time_eq_accepts_equal_strings() {
    assert!(constant_time_eq("", ""));
    assert!(constant_time_eq("root", "root"));
    assert!(constant_time_eq(
        "abcdefghijklmnopqrstuvwxyz01234",
        "abcdefghijklmnopqrstuvwxyz01234"
    ));
}

#[test]
fn constant_time_eq_rejects_oversize_inputs() {
    // A 33-byte string exceeds SECRET_MAX_LEN (32). Even when both sides
    // are identical and would compare equal after truncation, the oversize
    // guard must return false to prevent prefix-collision attacks.
    let oversize = "abcdefghijklmnopqrstuvwxyz0123456";
    assert!(!constant_time_eq(oversize, oversize));
    // One oversize, one within bounds — also false.
    assert!(!constant_time_eq(
        oversize,
        "abcdefghijklmnopqrstuvwxyz012345"
    ));
}
