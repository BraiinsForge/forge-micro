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

//! The request preamble every listener runs. [`check_csrf`] is a
//! separate step because a handler reads the body between the two: an
//! oversized body answers 413 without spending the single-use token.

use core::net::IpAddr;

use crate::allow_list::{self, DEFAULT_ALLOW_LIST};
use crate::auth::{self, AuthCtx};
use crate::csrf;

#[cfg(test)]
#[path = "tests/gate.rs"]
mod tests;

/// Per-request policy: the REST API admits no anonymous request, while
/// the dashboard must let a browser reach `/login`.
pub struct Policy {
    pub allow_anonymous: bool,
}

pub enum Rejection {
    NotAllowed,
    Unauthorized,
    CsrfInvalid,
}

/// The allow-list first, so an off-LAN probe never reaches the
/// credential check, then authentication. A header that fails either
/// way counts as no header, so the policy decides.
///
/// # Errors
///
/// [`Rejection::NotAllowed`] off the allow-list,
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

/// Consume the CSRF token of a session request. Takes the identity
/// [`gate_identity`] resolved, so the two steps cannot disagree. Only a
/// session cookie can be forged cross-site, so the other identities pass.
///
/// # Errors
///
/// [`Rejection::CsrfInvalid`] when the token is missing, unknown, or
/// another session's.
pub fn check_csrf(auth_ctx: &AuthCtx, csrf: Option<&str>) -> Result<(), Rejection> {
    if let AuthCtx::Session(session_id) = auth_ctx {
        let token = csrf.unwrap_or("");
        if token.is_empty() || !csrf::consume_csrf_token(session_id.as_str(), token) {
            return Err(Rejection::CsrfInvalid);
        }
    }
    Ok(())
}
