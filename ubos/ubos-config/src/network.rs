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

//! Network interface configuration, from the live schema version, read at
//! boot before the stack exists.

pub use crate::versions::live::{DNS_SERVERS, HOSTNAME_MAX, NetworkConfig, NetworkMode};

impl NetworkConfig {
    /// A non-empty hostname must be an RFC 1123 label. A static address
    /// and gateway must be non-zero, with a prefix of 1 to 32.
    ///
    /// # Errors
    ///
    /// The first failing rule, as an operator-readable string.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.hostname.is_empty() {
            if !self
                .hostname
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err("network.hostname must contain only letters, digits, and hyphens");
            }
            let bytes = self.hostname.as_bytes();
            if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
                return Err("network.hostname must not start or end with a hyphen");
            }
        }

        let NetworkMode::Static {
            address,
            prefix_len,
            gateway,
            ..
        } = &self.mode
        else {
            return Ok(());
        };

        if *address == [0u8; 4] {
            return Err("network.address must not be 0.0.0.0");
        }
        if *gateway == [0u8; 4] {
            return Err("network.gateway must not be 0.0.0.0");
        }
        if !(1..=32).contains(prefix_len) {
            return Err("network.prefix_len must be between 1 and 32");
        }

        Ok(())
    }
}
