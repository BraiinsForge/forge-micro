//! Tests for `session::{create,validate,invalidate}_session`.
//!
//! `SESSIONS` is a process-global table, so each test takes care to use a
//! locally-distinct token string and tears down at the end. Tests are
//! kept independent by deriving their tokens from the test function
//! name, avoiding ordering effects when the test binary runs them
//! concurrently.

extern crate std;

use heapless::String;

use crate::session::{create_session, invalidate_session, validate_session};

fn make_token(text: &str) -> String<32> {
    let mut token: String<32> = String::new();
    token
        .push_str(text)
        .expect("BUG: test token exceeds String<32>");
    token
}

#[test]
fn unknown_token_does_not_validate() {
    let result = embassy_futures::block_on(validate_session("not-a-real-token-12345"));
    assert!(!result);
}

#[test]
fn create_then_validate_returns_true() {
    let token = make_token("test-create-then-validate-tok-1");
    embassy_futures::block_on(create_session(token.clone()));
    let result = embassy_futures::block_on(validate_session(token.as_str()));
    assert!(result, "freshly-created session must validate");
    // Clean up so a later run starts from the same baseline.
    embassy_futures::block_on(invalidate_session(token.as_str()));
}

#[test]
fn invalidate_then_validate_returns_false() {
    let token = make_token("test-invalidate-then-validate1");
    embassy_futures::block_on(create_session(token.clone()));
    embassy_futures::block_on(invalidate_session(token.as_str()));
    let result = embassy_futures::block_on(validate_session(token.as_str()));
    assert!(!result, "invalidated session must not validate");
}

#[test]
fn invalidate_unknown_token_is_a_noop() {
    // Should not panic, should not affect any other session.
    embassy_futures::block_on(invalidate_session("never-existed-token-zzzzz"));
}

#[test]
fn validate_distinguishes_close_tokens() {
    // Two tokens that differ only in the last byte must not be
    // collapsed; constant_time_eq runs to completion so a final-byte
    // mismatch is still detected.
    let live = make_token("test-distinguishes-aa");
    let other = make_token("test-distinguishes-ab");
    embassy_futures::block_on(create_session(live.clone()));
    assert!(embassy_futures::block_on(validate_session(live.as_str())));
    assert!(!embassy_futures::block_on(validate_session(other.as_str())));
    embassy_futures::block_on(invalidate_session(live.as_str()));
}
