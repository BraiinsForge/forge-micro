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

//! Tests for `gate_identity` and `check_csrf`, covering the REST and
//! dashboard policies.

extern crate std;

use core::net::{IpAddr, Ipv4Addr};

use crate::auth::AuthCtx;
use crate::csprng::seed_csprng;
use crate::gate::{Policy, Rejection, check_csrf, gate_identity};

/// Seeds the CSPRNG with a key no other test module uses. Two modules
/// that reseed to the same state draw the same token strings, and a
/// token one of them consumes then fails the other's check.
fn ensure_seeded() {
    seed_csprng([0x77u8; 32]);
}

/// A LAN address that passes the default allow-list.
const LAN_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
/// A public address rejected by the default allow-list.
const WAN_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));

/// REST's policy: no anonymous request is admitted.
const REST_POLICY: Policy = Policy {
    allow_anonymous: false,
};

/// The dashboard's policy: anonymous is admitted so a browser reaches
/// `/login`.
const DASHBOARD_POLICY: Policy = Policy {
    allow_anonymous: true,
};

/// A fresh session and the cookie header that names it. The caller
/// seeds the CSPRNG once first, so two sessions in one test draw
/// distinct tokens.
fn session_cookie() -> (heapless::String<32>, heapless::String<40>) {
    let token = crate::csprng::random_token();
    crate::session::create_session(token.clone());
    let mut cookie = heapless::String::<40>::new();
    let _ = core::fmt::Write::write_fmt(&mut cookie, format_args!("session={token}"));
    (token, cookie)
}

#[test]
fn off_lan_peer_is_rejected_before_credentials_are_checked() {
    let outcome = gate_identity(WAN_IP, None, None, &REST_POLICY);
    assert!(matches!(outcome, Err(Rejection::NotAllowed)));
}

#[test]
fn rest_policy_rejects_anonymous_requests() {
    let outcome = gate_identity(LAN_IP, None, None, &REST_POLICY);
    assert!(matches!(outcome, Err(Rejection::Unauthorized)));
}

#[test]
fn rest_policy_admits_basic_auth() {
    crate::auth::init_credentials();
    // "root:root" in base64 is "cm9vdDpyb290"
    let outcome = gate_identity(LAN_IP, Some("Basic cm9vdDpyb290"), None, &REST_POLICY);
    assert!(matches!(outcome, Ok(AuthCtx::Basic)));
}

#[test]
fn wrong_basic_credentials_are_treated_as_anonymous() {
    crate::auth::init_credentials();
    // "root:wrong" in base64 is "cm9vdDp3cm9uZw=="
    let wrong = Some("Basic cm9vdDp3cm9uZw==");
    assert!(matches!(
        gate_identity(LAN_IP, wrong, None, &REST_POLICY),
        Err(Rejection::Unauthorized)
    ));
    assert!(matches!(
        gate_identity(LAN_IP, wrong, None, &DASHBOARD_POLICY),
        Ok(AuthCtx::Anonymous)
    ));
}

#[test]
fn dashboard_policy_admits_anonymous_requests() {
    let outcome = gate_identity(LAN_IP, None, None, &DASHBOARD_POLICY);
    assert!(matches!(outcome, Ok(AuthCtx::Anonymous)));
}

#[test]
fn a_session_with_no_csrf_token_is_rejected() {
    ensure_seeded();
    let (_, cookie) = session_cookie();
    let identity = gate_identity(LAN_IP, None, Some(cookie.as_str()), &DASHBOARD_POLICY)
        .unwrap_or(AuthCtx::Anonymous);
    assert!(matches!(identity, AuthCtx::Session(_)));
    assert!(matches!(
        check_csrf(&identity, None),
        Err(Rejection::CsrfInvalid)
    ));
}

#[test]
fn a_session_with_a_valid_csrf_token_passes() {
    ensure_seeded();
    let (token, cookie) = session_cookie();
    let csrf_token = crate::csrf::issue_csrf_token(token.as_str());
    let identity = gate_identity(LAN_IP, None, Some(cookie.as_str()), &DASHBOARD_POLICY)
        .unwrap_or(AuthCtx::Anonymous);
    assert!(check_csrf(&identity, Some(csrf_token.as_str())).is_ok());
}

#[test]
fn a_token_issued_for_another_session_is_rejected() {
    ensure_seeded();
    let (_, cookie) = session_cookie();
    let (other, _) = session_cookie();
    let foreign_token = crate::csrf::issue_csrf_token(other.as_str());
    let identity = gate_identity(LAN_IP, None, Some(cookie.as_str()), &DASHBOARD_POLICY)
        .unwrap_or(AuthCtx::Anonymous);
    assert!(matches!(
        check_csrf(&identity, Some(foreign_token.as_str())),
        Err(Rejection::CsrfInvalid)
    ));
}

#[test]
fn basic_and_anonymous_identities_are_never_csrf_checked() {
    assert!(check_csrf(&AuthCtx::Basic, None).is_ok());
    assert!(check_csrf(&AuthCtx::Anonymous, None).is_ok());
}

#[test]
fn a_page_load_does_not_consume_a_csrf_token() {
    ensure_seeded();
    // A plain page load only resolves the identity; the token stays
    // unspent for the mutating request that follows.
    let (token, cookie) = session_cookie();
    let csrf_token = crate::csrf::issue_csrf_token(token.as_str());
    let identity = gate_identity(LAN_IP, None, Some(cookie.as_str()), &DASHBOARD_POLICY)
        .unwrap_or(AuthCtx::Anonymous);
    assert!(matches!(identity, AuthCtx::Session(_)));
    assert!(check_csrf(&identity, Some(csrf_token.as_str())).is_ok());
}
