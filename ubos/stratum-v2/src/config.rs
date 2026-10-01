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

//! V2-specific session config. Constructed once by the caller (a
//! firmware boot path or a host probe) and handed to
//! `StratumV2Client::new` by reference.

use heapless::String;

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

/// Maximum byte length of the `user_identity` field. A typical V2
/// worker identity is `<bech32 address>.<worker name>`; a P2TR
/// address tops out at 62 chars, plus `.<workername>` of up to ~16
/// chars, comfortably under 128 bytes.
pub const MAX_USER_LEN: usize = 128;

/// `Sv2Config::new` rejects a worker identity that does not fit in
/// the fixed-size `user` buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTooLong;

/// Pool credentials and authentication material for a Stratum V2
/// session. `user` becomes the channel `user_identity` field on
/// `OpenStandardMiningChannel`. `pool_pubkey`, when present, is
/// used for the noise handshake's `ValidateSignature` authenticator;
/// `None` falls back to `ValidateNothing`, for unauthenticated
/// test-pool connections only.
///
/// `user` is owned as a `heapless::String` rather than borrowed, so
/// the config carries no borrow on its source string and callers can
/// hold it in a `StaticCell` or pass it by reference without fighting
/// `'static` lifetimes for the worker identity.
pub struct Sv2Config {
    pub user: String<MAX_USER_LEN>,
    pub pool_pubkey: Option<[u8; 32]>,
}

impl Sv2Config {
    /// Construct an `Sv2Config` from a `&str` worker identity.
    ///
    /// # Errors
    ///
    /// Returns `Err(UserTooLong)` if `user` is longer than
    /// [`MAX_USER_LEN`] bytes.
    pub fn new(user: &str, pool_pubkey: Option<[u8; 32]>) -> Result<Self, UserTooLong> {
        let mut owned = String::new();
        owned.push_str(user).map_err(|_| UserTooLong)?;
        Ok(Self {
            user: owned,
            pool_pubkey,
        })
    }
}
