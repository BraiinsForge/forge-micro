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

//! Tests for the session table. It is process-global, so each test
//! derives its tokens from its own name and tears down at the end.

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
    let result = validate_session("not-a-real-token-12345");
    assert!(!result);
}

#[test]
fn create_then_validate_returns_true() {
    let token = make_token("test-create-then-validate-tok-1");
    create_session(token.clone());
    let result = validate_session(token.as_str());
    assert!(result, "freshly-created session must validate");
    // Clean up so a later run starts from the same baseline.
    invalidate_session(token.as_str());
}

#[test]
fn invalidate_then_validate_returns_false() {
    let token = make_token("test-invalidate-then-validate1");
    create_session(token.clone());
    invalidate_session(token.as_str());
    let result = validate_session(token.as_str());
    assert!(!result, "invalidated session must not validate");
}

#[test]
fn invalidate_unknown_token_is_a_noop() {
    // Should not panic, should not affect any other session.
    invalidate_session("never-existed-token-zzzzz");
}

#[test]
fn validate_distinguishes_close_tokens() {
    let live = make_token("test-distinguishes-aa");
    let other = make_token("test-distinguishes-ab");
    create_session(live.clone());
    assert!(validate_session(live.as_str()));
    assert!(!validate_session(other.as_str()));
    invalidate_session(live.as_str());
}
