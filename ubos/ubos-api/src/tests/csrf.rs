//! Tests for `csrf::{issue,consume}_csrf_token`.
//!
//! `CSRF_TOKENS` is process-global, so each test uses a session id
//! string derived from its own name to stay independent of any prior
//! state in the table. `seed_csprng` must be called before
//! `issue_csrf_token`, so each test seeds with a fixed key.

extern crate std;

use crate::csprng::seed_csprng;
use crate::csrf::{consume_csrf_token, issue_csrf_token};

fn ensure_seeded() {
    // The seed value is irrelevant for these tests; any 32 bytes will
    // satisfy the `random_token` precondition.
    embassy_futures::block_on(seed_csprng([0x42u8; 32]));
}

#[test]
fn issued_token_consumes_with_matching_session_id() {
    ensure_seeded();
    let session_id = "test-csrf-issue-and-consume";
    let token = embassy_futures::block_on(issue_csrf_token(session_id));
    let accepted = embassy_futures::block_on(consume_csrf_token(session_id, token.as_str()));
    assert!(accepted, "freshly-issued token must consume successfully");
}

#[test]
fn issued_token_is_single_use() {
    ensure_seeded();
    let session_id = "test-csrf-single-use";
    let token = embassy_futures::block_on(issue_csrf_token(session_id));
    assert!(embassy_futures::block_on(consume_csrf_token(
        session_id,
        token.as_str()
    )));
    // Second consume on the same token must fail.
    assert!(
        !embassy_futures::block_on(consume_csrf_token(session_id, token.as_str())),
        "second consume on the same token must fail"
    );
}

#[test]
fn token_bound_to_wrong_session_rejected() {
    ensure_seeded();
    let issuing_session = "test-csrf-bind-issue-sid";
    let wrong_session = "test-csrf-bind-wrong-sid";
    let token = embassy_futures::block_on(issue_csrf_token(issuing_session));
    assert!(
        !embassy_futures::block_on(consume_csrf_token(wrong_session, token.as_str())),
        "token consumed under wrong session id must be rejected"
    );
    // The wrong-session attempt does not consume the token; the right
    // session can still use it.
    assert!(embassy_futures::block_on(consume_csrf_token(
        issuing_session,
        token.as_str()
    )));
}

#[test]
fn unknown_token_rejected() {
    let result = embassy_futures::block_on(consume_csrf_token(
        "test-csrf-unknown",
        "not-a-csrf-token-aaaaaaaaaaaa",
    ));
    assert!(!result);
}
