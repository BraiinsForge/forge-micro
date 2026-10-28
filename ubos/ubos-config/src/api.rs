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

//! HTTP API credentials configuration and validation.
//!
//! Re-exports [`ApiConfig`] for the HTTP API username and password from
//! the live version of the config schema, and defines its custom
//! [`Default`] impl and a [`validate`][ApiConfig::validate] method.

pub use crate::versions::live::ApiConfig;

impl Default for ApiConfig {
    fn default() -> Self {
        let mut username = heapless::String::new();
        let mut password = heapless::String::new();
        // "root" is 4 bytes; both fields have 32-byte capacity, so
        // push_str cannot return an error.
        username
            .push_str("root")
            .expect("BUG: \"root\" exceeds ApiConfig::username capacity");
        // Same invariant: 4-byte literal into a 32-byte heapless::String.
        password
            .push_str("root")
            .expect("BUG: \"root\" exceeds ApiConfig::password capacity");
        Self { username, password }
    }
}

impl ApiConfig {
    /// Validate the credentials.
    ///
    /// Username must be non-empty and consist entirely of printable
    /// ASCII characters. Password may be empty as the "keep existing"
    /// sentinel; if non-empty it must also be all printable ASCII.
    ///
    /// The empty-password exception is load-bearing for the wire
    /// round-trip: the `POST /api/config` handler in `ubos-api`
    /// substitutes the stored password when the incoming field is
    /// empty before calling this validator, so the SPA can edit the
    /// username without forcing the operator to re-type the password.
    /// Tightening this rule to reject empty passwords would break
    /// that round-trip; any tighter policy must land together with a
    /// matching update to the `rest.rs` substitution path.
    ///
    /// # Errors
    ///
    /// Returns the first validation error as a static
    /// operator-readable string.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.username.is_empty() {
            return Err("api.username must not be empty");
        }
        if !self.username.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("api.username must contain only printable ASCII characters");
        }
        if !self.password.is_empty() && !self.password.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("api.password must contain only printable ASCII characters");
        }
        Ok(())
    }
}
