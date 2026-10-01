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

//! HTTP API credentials, from the live schema version.

pub use crate::versions::live::ApiConfig;

impl Default for ApiConfig {
    fn default() -> Self {
        let mut username = heapless::String::new();
        let mut password = heapless::String::new();
        username
            .push_str("root")
            .expect("BUG: \"root\" exceeds ApiConfig::username capacity");
        password
            .push_str("root")
            .expect("BUG: \"root\" exceeds ApiConfig::password capacity");
        Self { username, password }
    }
}

impl ApiConfig {
    /// Both fields must be printable ASCII. An empty password is allowed:
    /// it means "keep the stored one", which the `POST /api/config`
    /// handler substitutes before calling this, so the SPA can edit the
    /// username without a re-typed password.
    ///
    /// # Errors
    ///
    /// The first failing rule, as an operator-readable string.
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
