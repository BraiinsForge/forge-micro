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

//! Build an `embassy-net` stack configuration from stored network
//! parameters.
//!
//! The config layer stores addresses as raw octets to stay free of the
//! link layer; this module converts them into the `embassy-net`/smoltcp
//! types the stack constructor expects. It takes primitive octet inputs
//! so board `main` functions can build the boot stack config before the
//! stack exists.
//!
//! `ubos-net` depends on `ubos-config` for the `run_network_control`
//! watch; that stays acyclic as long as `ubos-config` never depends on
//! the manager, mining, or stratum-client chain.

use embassy_net::{Config, Ipv4Address, Ipv4Cidr, StaticConfigV4};

/// Build a static IPv4 [`embassy_net::Config`] from octet inputs.
///
/// `address` and `prefix_len` become the interface [`Ipv4Cidr`],
/// `gateway` is set as the default route, and up to three `dns` entries
/// populate the resolver list; any beyond the third are dropped. The
/// caller is responsible for having validated the values, which
/// `ubos_config` does before persisting.
#[must_use]
pub fn static_ipv4(address: [u8; 4], prefix_len: u8, gateway: [u8; 4], dns: &[[u8; 4]]) -> Config {
    let mut dns_servers: heapless::Vec<Ipv4Address, 3> = heapless::Vec::new();
    for server in dns {
        if dns_servers.push(Ipv4Address::from(*server)).is_err() {
            break;
        }
    }
    Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::from(address), prefix_len),
        gateway: Some(Ipv4Address::from(gateway)),
        dns_servers,
    })
}
