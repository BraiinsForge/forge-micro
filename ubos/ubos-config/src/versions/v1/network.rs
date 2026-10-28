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

//! The network config of version 1.

/// Maximum length in bytes of a network hostname.
pub const HOSTNAME_MAX: usize = 32;

/// Selects the IP address assignment mode and carries the parameters
/// for each variant.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum NetworkMode {
    /// Address is assigned by a DHCP server on the local network.
    #[default]
    Dhcp,
    /// Address is configured statically by the operator.
    Static {
        /// IPv4 address as four octets.
        address: [u8; 4],
        /// Network prefix length (1–32).
        prefix_len: u8,
        /// Default gateway address.
        gateway: [u8; 4],
        /// DNS resolver addresses; up to three entries.
        dns: heapless::Vec<[u8; 4], 3>,
    },
}

/// Network interface parameters for both IP assignment and hostname.
///
/// Stored as part of [`crate::Config`] and applied at boot before the
/// network stack is initialised so the embassy-net stack sees the
/// correct mode on its first poll.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetworkConfig {
    /// IP assignment mode; defaults to DHCP.
    pub mode: NetworkMode,
    /// mDNS hostname for the device; empty means use the build-time
    /// default.
    pub hostname: heapless::String<HOSTNAME_MAX>,
}
