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

//! Network interface configuration and validation.
//!
//! Re-exports [`NetworkMode`] (DHCP vs. static assignment) and
//! [`NetworkConfig`] from the live version of the config schema, and
//! defines the validation of the network config. It is stored alongside
//! the rest of [`crate::Config`] and loaded at boot before the network
//! stack is created.

pub use crate::versions::live::{DNS_SERVERS, HOSTNAME_MAX, NetworkConfig, NetworkMode};

impl NetworkConfig {
    /// Validate the network configuration.
    ///
    /// An empty hostname is accepted as a sentinel for "use the
    /// build-time mDNS base name". A non-empty hostname must satisfy
    /// RFC-1123 label rules: each character must be an ASCII
    /// alphanumeric or a hyphen, and the first and last characters
    /// must not be hyphens.
    ///
    /// For [`NetworkMode::Static`], the address and gateway must be
    /// non-zero (`0.0.0.0` is not a valid host address), and
    /// `prefix_len` must be in the range 1–32.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a static operator-readable message on the
    /// first field that fails its rule.
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
