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

//! Composed request preamble: allow-list, authentication, and CSRF.
//!
//! [`gate_identity`] runs the allow-list and the credential check every
//! listener needs, in the same order, against a per-service [`Policy`].
//! [`check_csrf`] is the third step, taken separately because a handler
//! reads a mutating request's body between the two: an oversized body
//! answers 413 without spending the single-use token. The identity is
//! resolved once and handed to the CSRF check, so the two steps cannot
//! disagree about who the request is from.

use core::net::IpAddr;

use crate::allow_list::{self, DEFAULT_ALLOW_LIST};
use crate::auth::{self, AuthCtx};
use crate::csrf;

#[cfg(test)]
#[path = "tests/gate.rs"]
mod tests;

/// Per-request policy passed to [`gate_identity`].
///
/// A caller builds one of these from its own routing rules, since the
/// REST API admits no anonymous request while the dashboard must let an
/// unauthenticated browser reach `/login`.
pub struct Policy {
    /// Whether a request that carries no credentials is admitted as
    /// [`AuthCtx::Anonymous`] rather than rejected outright.
    pub allow_anonymous: bool,
}

/// Why the preamble rejected a request.
pub enum Rejection {
    /// The peer address is outside the configured allow-list.
    NotAllowed,
    /// No credentials were presented and the policy does not admit
    /// anonymous requests.
    Unauthorized,
    /// A CSRF token was required and missing, unknown, or issued for a
    /// different session.
    CsrfInvalid,
}

/// Run the allow-list and authentication checks for one request.
///
/// The allow-list is checked first, so an off-LAN probe never reaches
/// the credential check. Authentication follows. An `Authorization`
/// header that fails to parse or fails the credential compare is
/// treated the same as no header at all: the identity is
/// [`AuthCtx::Anonymous`], so a policy that admits anonymous requests
/// admits it. When the identity is anonymous and `policy.allow_anonymous`
/// is `false`, the request is rejected.
///
/// # Errors
///
/// [`Rejection::NotAllowed`] for a peer outside the allow-list,
/// [`Rejection::Unauthorized`] for an anonymous request the policy does
/// not admit.
pub fn gate_identity(
    peer_ip: IpAddr,
    authorization: Option<&str>,
    cookie: Option<&str>,
    policy: &Policy,
) -> Result<AuthCtx, Rejection> {
    if !allow_list::is_allowed(peer_ip, DEFAULT_ALLOW_LIST) {
        return Err(Rejection::NotAllowed);
    }

    let auth_ctx = auth::authenticate(authorization, cookie).unwrap_or(AuthCtx::Anonymous);

    if matches!(auth_ctx, AuthCtx::Anonymous) && !policy.allow_anonymous {
        return Err(Rejection::Unauthorized);
    }

    Ok(auth_ctx)
}

/// Consume and validate the CSRF token of a session-authenticated
/// request.
///
/// Takes the identity [`gate_identity`] resolved rather than resolving
/// it again, so a session whose lifetime lapses between the two calls
/// is still checked as the session it was admitted as. `Basic`
/// authenticated and anonymous identities pass unchecked, since only a
/// session cookie can be forged cross-site.
///
/// # Errors
///
/// [`Rejection::CsrfInvalid`] when the identity is a session and the
/// token is missing, unknown, or issued for a different session.
pub fn check_csrf(auth_ctx: &AuthCtx, csrf: Option<&str>) -> Result<(), Rejection> {
    if let AuthCtx::Session(session_id) = auth_ctx {
        let token = csrf.unwrap_or("");
        if token.is_empty() || !csrf::consume_csrf_token(session_id.as_str(), token) {
            return Err(Rejection::CsrfInvalid);
        }
    }
    Ok(())
}
